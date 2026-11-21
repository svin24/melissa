use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex as StdMutex,
};

use super::*;

struct Fixture {
    auth: Auth,
    clock: Arc<AtomicU64>,
    events: Arc<StdMutex<Vec<AuthEvent>>>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(Auth::in_memory())
    }

    fn with(auth: Auth) -> Self {
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let events = Arc::new(StdMutex::new(Vec::new()));
        let (c, e) = (clock.clone(), events.clone());
        let auth = auth
            .with_cost(Cost::Test)
            .with_clock(Arc::new(move || c.load(Ordering::SeqCst)))
            .with_events(Arc::new(move |ev: &AuthEvent| {
                e.lock().unwrap().push(ev.clone())
            }));
        Self {
            auth,
            clock,
            events,
        }
    }

    fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn actions(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.action)
            .collect()
    }

    fn claimed(&self) -> NewSession {
        let (code, _) = self.auth.setup_code().unwrap().unwrap();
        self.auth
            .claim(&code, "Ana", "owner-password", "198.51.100.1", "Firefox")
            .unwrap()
    }
}

#[test]
fn claim_needs_the_code_and_works_once() {
    let f = Fixture::new();
    assert!(!f.auth.has_owner());
    // No code asked for yet: nothing to claim with.
    assert_eq!(
        f.auth
            .claim("ABCDE-FGHJK", "ana", "owner-password", "a", "")
            .unwrap_err(),
        AuthError::SetupCodeExpired
    );
    let (code, expires) = f.auth.setup_code().unwrap().unwrap();
    assert_eq!(expires, 1_000_000 + SETUP_CODE_SECS);
    // The same code until it expires.
    assert_eq!(f.auth.setup_code().unwrap().unwrap().0, code);
    assert_eq!(
        f.auth
            .claim("WRONG-CODE2", "ana", "owner-password", "a", "")
            .unwrap_err(),
        AuthError::WrongSetupCode
    );
    // Case and dashes don't matter.
    let typed = code.to_lowercase().replace('-', " ");
    let s = f
        .auth
        .claim(&typed, "Ana", "owner-password", "a", "Firefox")
        .unwrap();
    assert_eq!(s.view.person.username, "ana");
    assert_eq!(s.view.person.role, Role::Owner);
    assert!(s.view.reauth_fresh);
    assert!(f.auth.has_owner());
    assert_eq!(f.auth.setup_code().unwrap(), None);
    assert_eq!(
        f.auth
            .claim(&code, "eve", "eve-password", "a", "")
            .unwrap_err(),
        AuthError::AlreadyClaimed
    );
    assert_eq!(f.actions(), vec!["auth.claim_failed", "auth.claim"]);
}

#[test]
fn setup_code_expires_and_burns_after_too_many_tries() {
    let f = Fixture::new();
    let (first, _) = f.auth.setup_code().unwrap().unwrap();
    f.advance(SETUP_CODE_SECS);
    assert_eq!(
        f.auth
            .claim(&first, "ana", "owner-password", "a", "")
            .unwrap_err(),
        AuthError::SetupCodeExpired
    );
    let (second, _) = f.auth.setup_code().unwrap().unwrap();
    assert_ne!(first, second);
    for i in 0..SETUP_CODE_TRIES {
        // Different sources, so only the code's own limit applies.
        let r = f
            .auth
            .claim("XXXXX-XXXXX", "ana", "owner-password", &format!("s{i}"), "");
        assert_eq!(r.unwrap_err(), AuthError::WrongSetupCode);
    }
    // Guessed at too often: even the right code no longer works.
    assert_eq!(
        f.auth
            .claim(&second, "ana", "owner-password", "z", "")
            .unwrap_err(),
        AuthError::SetupCodeExpired
    );
    assert_ne!(f.auth.setup_code().unwrap().unwrap().0, second);
}

#[test]
fn claim_from_one_address_slows_down() {
    let f = Fixture::new();
    f.auth.setup_code().unwrap();
    for _ in 0..SETUP_CODE_TRIES {
        let _ = f
            .auth
            .claim("XXXXX-XXXXX", "ana", "owner-password", "a", "");
    }
    // A new code (the old one is used up); the address still has its failures.
    f.auth.setup_code().unwrap();
    for _ in SETUP_CODE_TRIES..=limits::PER_SOURCE.free {
        let _ = f
            .auth
            .claim("XXXXX-XXXXX", "ana", "owner-password", "a", "");
    }
    assert!(matches!(
        f.auth
            .claim("XXXXX-XXXXX", "ana", "owner-password", "a", "")
            .unwrap_err(),
        AuthError::SlowDown { .. }
    ));
}

#[test]
fn login_checks_the_password_and_locks_out() {
    let f = Fixture::new();
    f.claimed();
    let s = f
        .auth
        .login("ANA ", "owner-password", "198.51.100.2", "Safari")
        .unwrap();
    assert_eq!(s.view.person.username, "ana");
    assert_eq!(
        f.auth
            .login("ana", "nope-nope", "198.51.100.2", "")
            .unwrap_err(),
        AuthError::WrongCredentials
    );
    // An unknown name gets the same answer.
    assert_eq!(
        f.auth
            .login("nobody", "nope-nope", "198.51.100.3", "")
            .unwrap_err(),
        AuthError::WrongCredentials
    );
    // Three free failures, then a wait, then a lockout at ten.
    for _ in 0..2 {
        f.auth.login("ana", "nope-nope", "a", "").unwrap_err();
    }
    assert!(f.auth.login("ana", "owner-password", "a", "").is_ok());
    for i in 0..limits::PER_USER.lock {
        f.advance(61);
        let e = f
            .auth
            .login("ana", "nope-nope", &format!("10.0.0.{i}"), "")
            .unwrap_err();
        assert_eq!(e, AuthError::WrongCredentials, "try {i}");
    }
    f.advance(61);
    // Locked: even the right password is refused for now.
    assert!(matches!(
        f.auth.login("ana", "owner-password", "b", ""),
        Err(AuthError::Locked { .. })
    ));
    f.advance(LOCKOUT_SECS);
    assert!(f.auth.login("ana", "owner-password", "b", "").is_ok());
    let actions = f.actions();
    assert!(actions.contains(&"auth.login"));
    assert!(actions.contains(&"auth.login_failed"));
    assert_eq!(actions.iter().filter(|a| **a == "auth.lockout").count(), 1);
}

#[test]
fn slow_down_refuses_without_checking() {
    let f = Fixture::new();
    f.claimed();
    for _ in 0..4 {
        let _ = f.auth.login("ana", "nope-nope", "a", "");
    }
    assert!(matches!(
        f.auth.login("ana", "owner-password", "a", ""),
        Err(AuthError::SlowDown { .. })
    ));
}

#[test]
fn sessions_expire_and_can_be_revoked() {
    let f = Fixture::new();
    let first = f.claimed();
    let owner = first.view.person.id.clone();
    let second = f.auth.login("ana", "owner-password", "a", "Phone").unwrap();
    assert_eq!(f.auth.sessions(&owner).len(), 2);
    let view = f.auth.session(&first.token).unwrap();
    assert_eq!(view.csrf, first.view.csrf);
    assert!(f.auth.session("forged").is_none());

    // Re-auth goes stale after five minutes.
    f.advance(REAUTH_SECS);
    assert!(!f.auth.session(&first.token).unwrap().reauth_fresh);
    assert_eq!(
        f.auth.reauth(&first.token, "wrong-one", "a").unwrap_err(),
        AuthError::WrongCredentials
    );
    f.auth.reauth(&first.token, "owner-password", "a").unwrap();
    assert!(f.auth.session(&first.token).unwrap().reauth_fresh);

    // Revoke one by id.
    assert!(f.auth.revoke_session(&owner, &second.view.id, "a"));
    assert!(f.auth.session(&second.token).is_none());
    assert!(!f.auth.revoke_session("someone-else", &first.view.id, "a"));

    // Idle timeout: used within the idle window it lives on, unused it ends.
    f.advance(SESSION_IDLE_SECS - 1);
    assert!(f.auth.session(&first.token).is_some());
    f.advance(SESSION_IDLE_SECS);
    assert!(f.auth.session(&first.token).is_none());

    // Absolute lifetime, however busy.
    let s = f.auth.login("ana", "owner-password", "a", "").unwrap();
    for _ in 0..(SESSION_MAX_SECS / (24 * 3600)) {
        f.advance(24 * 3600);
        f.auth.session(&s.token);
    }
    assert!(f.auth.session(&s.token).is_none());

    // Sign out everywhere.
    let a = f.auth.login("ana", "owner-password", "a", "").unwrap();
    let b = f.auth.login("ana", "owner-password", "a", "").unwrap();
    assert_eq!(f.auth.revoke_all(&owner, "a"), 2);
    assert!(f.auth.session(&a.token).is_none() && f.auth.session(&b.token).is_none());
    // Sign out.
    let c = f.auth.login("ana", "owner-password", "a", "").unwrap();
    f.auth.sign_out(&c.token, "a");
    assert!(f.auth.session(&c.token).is_none());
}

#[test]
fn invites_are_single_use_and_expire() {
    let f = Fixture::new();
    let owner = f.claimed().view.person;
    let inv = f.auth.create_invite(&owner.id, "a").unwrap();
    assert_eq!(f.auth.invites().len(), 1);
    assert!(f.auth.invite(&inv.token).is_some());
    assert!(f.auth.invite("forged").is_none());
    // Bad input keeps the invite usable.
    assert!(matches!(
        f.auth
            .accept_invite(&inv.token, "Bo b", "kid-password", "b", ""),
        Err(AuthError::UsernameInvalid(_))
    ));
    assert_eq!(
        f.auth
            .accept_invite(&inv.token, "ana", "kid-password", "b", "")
            .unwrap_err(),
        AuthError::UsernameTaken
    );
    let s = f
        .auth
        .accept_invite(&inv.token, "bob", "kid-password", "b", "Tablet")
        .unwrap();
    assert_eq!(s.view.person.role, Role::Member);
    assert_eq!(
        f.auth
            .accept_invite(&inv.token, "carl", "kid-password", "b", "")
            .unwrap_err(),
        AuthError::InviteInvalid
    );
    assert!(f.auth.invites().is_empty());

    // Expiry.
    let late = f.auth.create_invite(&owner.id, "a").unwrap();
    f.advance(INVITE_SECS);
    assert!(f.auth.invite(&late.token).is_none());
    assert_eq!(
        f.auth
            .accept_invite(&late.token, "carl", "kid-password", "b", "")
            .unwrap_err(),
        AuthError::InviteInvalid
    );
    // Revoked.
    let gone = f.auth.create_invite(&owner.id, "a").unwrap();
    assert!(f.auth.revoke_invite(&gone.info.id, &owner.id, "a"));
    assert!(f.auth.invite(&gone.token).is_none());
}

#[test]
fn reset_links_set_a_new_password_and_sign_out() {
    let f = Fixture::new();
    let owner = f.claimed().view.person;
    let inv = f.auth.create_invite(&owner.id, "a").unwrap();
    let bob = f
        .auth
        .accept_invite(&inv.token, "bob", "kid-password", "b", "")
        .unwrap();
    let reset = f
        .auth
        .reset_link(&owner.id, &bob.view.person.id, "a")
        .unwrap();
    assert_eq!(reset.info.username.as_deref(), Some("bob"));
    // A second link replaces the first.
    let reset2 = f
        .auth
        .reset_link(&owner.id, &bob.view.person.id, "a")
        .unwrap();
    assert!(f.auth.invite(&reset.token).is_none());
    let s = f
        .auth
        .accept_invite(&reset2.token, "ignored", "new-kid-password", "b", "")
        .unwrap();
    assert_eq!(s.view.person.username, "bob");
    assert!(f.auth.session(&bob.token).is_none(), "old sessions end");
    assert!(f.auth.login("bob", "kid-password", "c", "").is_err());
    assert!(f.auth.login("bob", "new-kid-password", "c", "").is_ok());

    // The host's owner reset.
    let o = f.auth.owner_reset().unwrap();
    assert_eq!(o.info.expires - o.info.created, OWNER_RESET_SECS);
    f.auth
        .accept_invite(&o.token, "", "new-owner-password", "h", "")
        .unwrap();
    assert!(f.auth.login("ana", "new-owner-password", "c", "").is_ok());
}

#[test]
fn members_can_be_removed_but_never_the_owner() {
    let f = Fixture::new();
    let owner = f.claimed().view.person;
    let inv = f.auth.create_invite(&owner.id, "a").unwrap();
    let bob = f
        .auth
        .accept_invite(&inv.token, "bob", "kid-password", "b", "")
        .unwrap();
    assert_eq!(
        f.auth.remove_member(&owner.id, &owner.id, "a").unwrap_err(),
        AuthError::OwnerProtected
    );
    let gone = f
        .auth
        .remove_member(&bob.view.person.id, &owner.id, "a")
        .unwrap();
    assert_eq!(gone.username, "bob");
    assert!(f.auth.session(&bob.token).is_none());
    assert_eq!(f.auth.people().len(), 1);
}

#[test]
fn change_password_signs_out_other_sessions() {
    let f = Fixture::new();
    let a = f.claimed();
    let b = f.auth.login("ana", "owner-password", "a", "").unwrap();
    assert_eq!(
        f.auth
            .change_password(&a.token, "wrong-pass", "brand-new-pass", "a")
            .unwrap_err(),
        AuthError::WrongCredentials
    );
    f.auth
        .change_password(&a.token, "owner-password", "brand-new-pass", "a")
        .unwrap();
    assert!(f.auth.session(&a.token).is_some());
    assert!(f.auth.session(&b.token).is_none());
}

#[test]
fn usernames_and_passwords_are_checked() {
    assert_eq!(check_username(" Ana.B ").unwrap(), "ana.b");
    assert!(check_username("").is_err());
    assert!(check_username("-ana").is_err());
    assert!(check_username("a b").is_err());
    assert!(check_username("host").is_err());
    assert!(check_username(&"a".repeat(33)).is_err());
    assert!(check_password("short", "ana").is_err());
    assert!(check_password("ana-is-long", "ana").is_ok());
    assert!(check_password("anaANAana", "anaanaana").is_err());
}

#[test]
fn the_file_is_private_and_keeps_only_hashes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("melissa-auth-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let f = Fixture::with(Auth::open(&dir).unwrap());
    let s = f.claimed();
    let inv = f.auth.create_invite(&s.view.person.id, "a").unwrap();
    let file = dir.join(FILE_NAME);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(&s.token) && !text.contains(&inv.token));
    assert!(!text.contains("owner-password"));
    assert!(text.contains("$argon2id$"));
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    // Reopened: the session and the owner are still there.
    let again = Auth::open(&dir).unwrap().with_clock(Arc::new(|| 1_000_000));
    assert!(again.has_owner());
    assert!(again.session(&s.token).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
