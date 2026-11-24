use super::*;

#[test]
fn paths_are_public_member_or_owner() {
    for p in [
        "/login",
        "/claim",
        "/healthz",
        "/reset-status",
        "/assets/melissa.css",
        "/invite/abc",
        "/mcp/tok",
    ] {
        assert_eq!(need(p), Need::Public, "{p}");
    }
    for p in [
        "/",
        "/chat",
        "/ui/stream",
        "/api/lead/status",
        "/settings/agents",
        "/settings/account",
        "/settings/developer",
        "/settings/about",
        "/settings/about/rust-crates",
        "/reauth",
        "/logout",
        "/api/host/setup-code",
        "/loginx",
        "/settings/peoplex",
    ] {
        assert_eq!(need(p), Need::Member, "{p}");
    }
    for p in [
        "/settings/people",
        "/ui/people/invites",
        "/settings/providers",
        "/settings/providers/add",
        "/ui/settings/providers/claude/key",
        "/api/settings/providers",
        "/settings/apps",
        "/ui/settings/apps",
        "/settings/tailscale",
        "/ui/settings/tailscale/sign-in",
        "/settings/updates",
        "/api/settings/updates/apply",
        "/ui/settings/restart",
        "/ui/apps/nextcloud/force-delete",
        "/api/apps/nextcloud/force-delete",
        "/settings/reset",
        "/ui/settings/reset",
        "/api/settings/reset",
        "/api/usage/providers",
        "/api/threads",
        "/api/threads/lead/d-0a1b2c3d.a1",
    ] {
        assert_eq!(need(p), Need::Owner, "{p}");
    }
}

#[test]
fn sensitive_actions_are_mutations_only() {
    let post = Method::POST;
    for p in [
        "/ui/apps/x/setup-password/reveal",
        "/ui/apps/x/setup-password/dismiss",
        "/ui/apps/x/force-delete",
        "/ui/settings/apps",
        "/ui/settings/tailscale/sign-in",
        "/ui/settings/tailscale/enable",
        "/ui/settings/providers/claude",
        "/api/settings/providers/claude",
        "/ui/people/invites",
        "/ui/settings/reset",
        "/api/settings/reset",
    ] {
        assert!(sensitive(&post, p), "{p}");
        assert!(!sensitive(&Method::GET, p), "{p}");
    }
    for p in [
        "/ui/messages",
        "/ui/apps/x/setup-password",
        "/ui/settings/files",
        "/ui/account/password",
    ] {
        assert!(!sensitive(&post, p), "{p}");
    }
}

#[test]
fn next_stays_on_this_site() {
    assert_eq!(safe_next(Some("/settings?x=1")), "/settings?x=1");
    for bad in [
        "//evil.example",
        "https://evil.example",
        "/\\evil",
        "evil",
        "/login?next=/x",
        "/reauth",
    ] {
        assert_eq!(safe_next(Some(bad)), "/", "{bad}");
    }
    assert_eq!(safe_next(None), "/");
}

#[test]
fn cookies_are_read_by_either_name() {
    let mut h = HeaderMap::new();
    h.insert(
        header::COOKIE,
        HeaderValue::from_static("a=1; melissa_session=tok; b=2"),
    );
    assert_eq!(session_cookie(&h).as_deref(), Some("tok"));
    let mut h = HeaderMap::new();
    h.insert(
        header::COOKIE,
        HeaderValue::from_static("__Host-melissa_session=sec"),
    );
    assert_eq!(session_cookie(&h).as_deref(), Some("sec"));
    assert_eq!(session_cookie(&HeaderMap::new()), None);
    // Plain HTTP: no prefix, no Secure; HTTPS: both.
    let plain = set_cookie(&HeaderMap::new(), Some("t"));
    assert_eq!(
        plain,
        "melissa_session=t; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"
    );
    let mut tls = HeaderMap::new();
    tls.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    let secure = set_cookie(&tls, Some("t"));
    assert!(secure
        .to_str()
        .unwrap()
        .starts_with("__Host-melissa_session=t; Path=/;"));
    assert!(secure.to_str().unwrap().ends_with("; Secure"));
    assert!(set_cookie(&tls, None)
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
}

#[test]
fn multipart_token_must_be_the_first_part() {
    let good = b"--B\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\ntok\r\n--B\r\n";
    assert!(multipart_token_ok(good, "B", "tok"));
    assert!(!multipart_token_ok(good, "B", "other"));
    assert!(!multipart_token_ok(good, "C", "tok"));
    let second = b"--B\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\nhi\r\n--B\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\ntok\r\n";
    assert!(!multipart_token_ok(second, "B", "tok"));
}

#[test]
fn browsers_get_short_labels() {
    let mut h = HeaderMap::new();
    h.insert(
        header::USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0",
        ),
    );
    assert_eq!(browser_label(&h), "Firefox on Linux");
    h.insert(
        header::USER_AGENT,
        HeaderValue::from_static("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Version/18.0 Mobile/15E148 Safari/604.1"),
    );
    assert_eq!(browser_label(&h), "Safari on iOS");
    assert_eq!(browser_label(&HeaderMap::new()), "Browser");
}
