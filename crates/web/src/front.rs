//! The web UI's listeners and what each may trust (MVP #1, DECISIONS 2026-11-28 "HTTPS access
//! from other devices"; research `docs/design/HTTPS-ACCESS-2026-11-28.md`).
//!
//! Every TCP listener runs [`layer`] before anything else:
//! - **Forwarded headers.** `X-Forwarded-Proto`, `-For`, `-Host`, `Forwarded`, `X-Real-IP`
//!   and Tailscale's identity headers are taken out of every request. Only a listener that
//!   trusts its proxy puts back what the proxy said: `X-Forwarded-Proto: https`, the client
//!   address (the last `X-Forwarded-For` entry, which the proxy appended) as the peer address
//!   the sign-in limits and the audit use (PLAN Q87a), and on Melissa's own `tailscale serve`
//!   listener the Tailscale login (shown and audited with the browser's name, never used to
//!   sign in). So [`crate::auth::https`] can believe the header.
//! - **Host names** (against DNS rebinding): a request must name this device as `localhost`,
//!   a loopback address, this device's MagicDNS name (Melissa's own Tailscale), a name from
//!   `[web] hosts`, or (only where the forwarded headers are trusted and say HTTPS, for the
//!   host's own `tailscale serve`) a `*.ts.net` name. Anything else gets 421.
//! - **HSTS** only on Melissa's own `tailscale serve` origin (its MagicDNS name over HTTPS),
//!   for a day, without `includeSubDomains` or `preload`.
//!
//! Listeners: the main one ([`Front::direct`], `listen`, 127.0.0.1:8080; the container's UI
//! forward and SSH tunnels arrive here) trusts forwarded headers only when told to (the
//! host's own `tailscale serve` in front of it, `[web] trust_forwarded`); the second one
//! ([`Front::tailscale_serve`], `[web] proxy_listen`, 127.0.0.1:8082) is the target of
//! Melissa's own `tailscale serve` and nothing else.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use melissa_settings::tailscale::TailscaleControl;

/// Headers a proxy sets; taken out of every request (see the module docs).
const FORWARDED: &[&str] = &[
    "x-forwarded-proto",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-port",
    "x-forwarded-server",
    "forwarded",
    "x-real-ip",
];
/// Tailscale's identity headers (`tailscale serve` adds them for tailnet users).
const TAILSCALE_IDENTITY: &[&str] = &[
    "tailscale-user-login",
    "tailscale-user-name",
    "tailscale-user-profile-pic",
    "tailscale-headers-info",
    "tailscale-app-capabilities",
];
/// The tailnet login of the person behind a request through Melissa's own `tailscale serve`
/// (shown with the browser's name and audited; never a way to sign in).
pub(crate) const TAILSCALE_LOGIN: &str = "tailscale-user-login";
/// Set (after every incoming copy is removed) on requests through Melissa's own `tailscale
/// serve` listener only.
pub(crate) const VIA: &str = "x-melissa-via";
const VIA_SERVE: &str = "tailscale-serve";
/// HSTS for the ts.net origin: one day, this host name only.
const HSTS: &str = "max-age=86400";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Direct,
    TailscaleServe,
}

/// What a listener trusts and which host names it answers to.
#[derive(Clone)]
pub struct Front {
    kind: Kind,
    trust_forwarded: bool,
    hosts: Arc<Vec<String>>,
    tailscale: Option<TailscaleControl>,
}

impl Front {
    /// The main listener. `trust_forwarded`: an HTTPS proxy on this host (the host's own
    /// `tailscale serve`) sits in front of it. `hosts`: more names it answers to
    /// (`[web] hosts`). `tailscale`: Melissa's own, for its MagicDNS name.
    pub fn direct(
        trust_forwarded: bool,
        hosts: Vec<String>,
        tailscale: Option<TailscaleControl>,
    ) -> Self {
        Self {
            kind: Kind::Direct,
            trust_forwarded,
            hosts: Arc::new(normalize(hosts)),
            tailscale,
        }
    }

    /// The loopback listener only Melissa's own `tailscale serve` forwards to.
    pub fn tailscale_serve(tailscale: TailscaleControl) -> Self {
        Self {
            kind: Kind::TailscaleServe,
            trust_forwarded: true,
            hosts: Arc::new(Vec::new()),
            tailscale: Some(tailscale),
        }
    }

    async fn own_name(&self) -> Option<String> {
        match &self.tailscale {
            Some(t) => t.own_dns_name().await,
            None => None,
        }
    }
}

fn normalize(hosts: Vec<String>) -> Vec<String> {
    hosts
        .into_iter()
        .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// Serve the UI on TCP behind [`layer`]. The peer address feeds the failed sign-in limits
/// and the audit.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    front: Front,
) -> std::io::Result<()> {
    let app = app.layer(middleware::from_fn_with_state(front, layer));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

/// The host name of a request (no port, lower case, no trailing dot).
pub(crate) fn host_name(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::HOST)?.to_str().ok()?;
    let name = crate::state::host_name_of(raw)?;
    Some(name.trim_end_matches('.').to_ascii_lowercase())
}

/// `localhost`, `*.localhost`, 127.0.0.0/8 or `[::1]`.
pub(crate) fn is_loopback_name(name: &str) -> bool {
    if name == "localhost" || name.ends_with(".localhost") {
        return true;
    }
    let bare = name.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The request names this device by a loopback name (or names nothing: the host socket and
/// tests). A browser on this device, an SSH tunnel.
pub(crate) fn loopback_host(headers: &HeaderMap) -> bool {
    match headers.get(header::HOST) {
        None => true,
        Some(_) => host_name(headers).is_some_and(|h| is_loopback_name(&h)),
    }
}

/// The request came through Melissa's own `tailscale serve` (set by [`layer`] only).
pub(crate) fn via_tailscale_serve(headers: &HeaderMap) -> bool {
    headers.get(VIA).is_some_and(|v| v == VIA_SERVE)
}

/// The last `X-Forwarded-For` entry: the address the proxy itself saw.
fn forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    let last = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .rfind(|s| !s.is_empty())?;
    last.parse().ok()
}

/// A tailnet login as shown next to the browser's name: printable ASCII, at most 100.
fn clean_login(v: &HeaderValue) -> Option<HeaderValue> {
    let s = v.to_str().ok()?.trim();
    let ok = !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\');
    ok.then(|| HeaderValue::from_str(s).ok()).flatten()
}

async fn layer(State(front): State<Front>, mut req: Request, next: Next) -> Response {
    let h = req.headers();
    let trusted = front.trust_forwarded;
    let https = trusted
        && h.get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"));
    let client = if trusted { forwarded_for(h) } else { None };
    let login = match front.kind {
        Kind::TailscaleServe => h.get(TAILSCALE_LOGIN).and_then(clean_login),
        Kind::Direct => None,
    };
    let h = req.headers_mut();
    for name in FORWARDED.iter().chain(TAILSCALE_IDENTITY).chain(&[VIA]) {
        h.remove(*name);
    }
    if https {
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    }
    if front.kind == Kind::TailscaleServe {
        h.insert(VIA, HeaderValue::from_static(VIA_SERVE));
        if let Some(l) = login {
            h.insert(TAILSCALE_LOGIN, l);
        }
    }
    if let Some(ip) = client {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 0)));
    }

    let host = host_name(req.headers());
    let own = match front.kind {
        // Only names that can be this device cost a status read.
        _ if host.as_deref().is_some_and(is_loopback_name) => None,
        _ => front.own_name().await,
    };
    let allowed = host.as_deref().is_some_and(|name| {
        is_loopback_name(name)
            || front.hosts.iter().any(|h| h == name)
            || own.as_deref() == Some(name)
            || (front.kind == Kind::Direct && https && name.ends_with(".ts.net"))
    });
    if !allowed {
        tracing::debug!(host = ?host, "refused a request for another host name");
        return (
            StatusCode::MISDIRECTED_REQUEST,
            crate::pages::error_page(
                "Wrong address",
                "Melissa doesn't answer at this address. Open her on this device, or at the \
                 address in Settings → Tailscale.",
            ),
        )
            .into_response();
    }
    let hsts = front.kind == Kind::TailscaleServe && https && own.is_some() && own == host;
    let mut res = next.run(req).await;
    if hsts {
        res.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(HSTS),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get};
    use tower::ServiceExt;

    fn echo() -> Router {
        Router::new().route(
            "/",
            get(
                |headers: HeaderMap, peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>| async move {
                    let show = |n: &str| {
                        headers
                            .get(n)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("-")
                            .to_string()
                    };
                    format!(
                        "proto={} via={} login={} xff={} peer={}",
                        show("x-forwarded-proto"),
                        show(VIA),
                        show(TAILSCALE_LOGIN),
                        show("x-forwarded-for"),
                        peer.map(|p| p.0 .0.ip().to_string()).unwrap_or("-".into()),
                    )
                },
            ),
        )
    }

    async fn ask(
        front: Front,
        host: &str,
        extra: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        let app = echo().layer(middleware::from_fn_with_state(front, layer));
        let mut b = axum::http::Request::get("/").header(header::HOST, host);
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        let res = app.oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let body = http_body_util::BodyExt::collect(res.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    const SPOOF: &[(&str, &str)] = &[
        ("x-forwarded-proto", "https"),
        ("x-forwarded-for", "203.0.113.9"),
        ("tailscale-user-login", "mallory@example.com"),
        (VIA, VIA_SERVE),
    ];

    #[tokio::test]
    async fn the_main_listener_strips_what_a_proxy_would_say() {
        let front = Front::direct(false, vec![], None);
        let (s, h, body) = ask(front, "127.0.0.1:8080", SPOOF).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            body, "proto=- via=- login=- xff=- peer=127.0.0.1",
            "nothing forged survives"
        );
        assert!(h.get(header::STRICT_TRANSPORT_SECURITY).is_none());
    }

    #[tokio::test]
    async fn host_names_are_checked() {
        let front = Front::direct(false, vec!["Melissa.Example.com".into()], None);
        for ok in [
            "127.0.0.1:8080",
            "localhost:9000",
            "[::1]:8080",
            "127.0.0.2",
            "melissa.example.com",
        ] {
            assert_eq!(ask(front.clone(), ok, &[]).await.0, StatusCode::OK, "{ok}");
        }
        for bad in [
            "attacker.example:8080",
            "192.0.2.7:8080",
            "box.example-tailnet.ts.net",
            "",
            "evil host",
        ] {
            let (s, _, body) = ask(front.clone(), bad, &[]).await;
            assert_eq!(s, StatusCode::MISDIRECTED_REQUEST, "{bad}");
            assert!(
                body.contains("doesn&#39;t answer") || body.contains("doesn't answer"),
                "{body}"
            );
        }
        // A ts.net name only through a trusted HTTPS proxy (the host's own `tailscale serve`).
        let (s, _, _) = ask(front, "box.example-tailnet.ts.net", SPOOF).await;
        assert_eq!(s, StatusCode::MISDIRECTED_REQUEST, "spoofed: not trusted");
        let trusting = Front::direct(true, vec![], None);
        let (s, h, body) = ask(
            trusting.clone(),
            "box.example-tailnet.ts.net",
            &[
                ("x-forwarded-proto", "https"),
                ("x-forwarded-for", "198.51.100.1, 100.64.0.9"),
            ],
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body, "proto=https via=- login=- xff=- peer=100.64.0.9");
        assert!(
            h.get(header::STRICT_TRANSPORT_SECURITY).is_none(),
            "not Melissa's origin"
        );
        let (s, _, _) = ask(trusting, "box.example-tailnet.ts.net", &[]).await;
        assert_eq!(s, StatusCode::MISDIRECTED_REQUEST, "plain HTTP: no ts.net");
    }

    /// Melissa's own Tailscale, connected as `melissa-box.example-tailnet.ts.net`.
    struct Connected;

    impl melissa_settings::tailscale::Cli for Connected {
        fn status(&self) -> melissa_agent::BoxFuture<'_, Result<String, String>> {
            Box::pin(async {
                Ok(serde_json::json!({
                    "BackendState": "Running",
                    "Self": {"HostName": "melissa-box",
                             "DNSName": "melissa-box.example-tailnet.ts.net.",
                             "TailscaleIPs": ["100.64.0.7"]},
                    "CurrentTailnet": {"MagicDNSEnabled": true},
                })
                .to_string())
            })
        }
        fn start_login<'a>(
            &'a self,
            _: &'a str,
        ) -> melissa_agent::BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn logout(&self) -> melissa_agent::BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    impl melissa_settings::tailscale::Service for Connected {
        fn start(&self) -> melissa_agent::BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn stop(&self) -> melissa_agent::BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn own_tailscale(tag: &str) -> TailscaleControl {
        use melissa_settings::tailscale as ts;
        let dir = std::env::temp_dir().join(format!("melissa-front-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ts::FILE_NAME), "enabled = true\n").unwrap();
        ts::TailscaleControl::new(ts::Parts {
            management: ts::Management::Melissa,
            setting: dir.join(ts::FILE_NAME),
            node_name: "melissa-box".into(),
            serve_target: Some("http://127.0.0.1:8082".into()),
            ui_port: 8080,
            cli: Arc::new(Connected),
            service: Arc::new(Connected),
            audit: None,
            wait: std::time::Duration::from_millis(100),
        })
    }

    const OWN: &str = "melissa-box.example-tailnet.ts.net";

    #[tokio::test]
    async fn the_tailscale_serve_listener_believes_its_proxy() {
        let front = Front::tailscale_serve(own_tailscale("serve"));
        let (s, h, body) = ask(
            front.clone(),
            OWN,
            &[
                ("x-forwarded-proto", "https"),
                ("x-forwarded-for", "203.0.113.9, 100.64.0.9"),
                ("tailscale-user-login", "ana@example.com"),
                (VIA, "forged"),
            ],
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            body, "proto=https via=tailscale-serve login=ana@example.com xff=- peer=100.64.0.9",
            "the proxy's view; the client's own XFF entry is ignored"
        );
        assert_eq!(h[header::STRICT_TRANSPORT_SECURITY], "max-age=86400");
        // Another ts.net name is not this device.
        let (s, _, _) = ask(
            front.clone(),
            "other-box.example-tailnet.ts.net",
            &[("x-forwarded-proto", "https")],
        )
        .await;
        assert_eq!(s, StatusCode::MISDIRECTED_REQUEST);
        // Loopback (a probe on the box): no HSTS.
        let (s, h, _) = ask(front, "127.0.0.1:8082", &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert!(h.get(header::STRICT_TRANSPORT_SECURITY).is_none());
    }

    #[tokio::test]
    async fn the_main_listener_answers_to_the_own_name_without_trusting_anything() {
        let front = Front::direct(false, vec![], Some(own_tailscale("direct")));
        let (s, h, body) = ask(front, OWN, SPOOF).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body, "proto=- via=- login=- xff=- peer=127.0.0.1");
        assert!(h.get(header::STRICT_TRANSPORT_SECURITY).is_none());
    }

    #[test]
    fn app_links_never_use_the_https_name() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static(OWN));
        assert_eq!(crate::ui::host(&h), Some(OWN));
        h.insert(VIA, HeaderValue::from_static(VIA_SERVE));
        assert_eq!(
            crate::ui::host(&h),
            None,
            "HSTS would turn http:// links into https"
        );
    }

    #[test]
    fn loopback_names() {
        for yes in [
            "localhost",
            "a.localhost",
            "127.0.0.1",
            "127.9.9.9",
            "[::1]",
        ] {
            assert!(is_loopback_name(yes), "{yes}");
        }
        for no in ["localhost.example.com", "10.0.0.1", "[::2]", "100.64.0.1"] {
            assert!(!is_loopback_name(no), "{no}");
        }
        let mut h = HeaderMap::new();
        assert!(loopback_host(&h), "no Host: the host socket, tests");
        h.insert(header::HOST, HeaderValue::from_static("192.0.2.7:8080"));
        assert!(!loopback_host(&h));
    }
}
