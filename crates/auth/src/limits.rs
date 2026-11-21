//! Failed sign-in limits: per username and per source address, a slow-down after a few
//! failures, then a temporary lockout. Kept in memory (a restart forgets them; only the
//! host can restart Melissa).

use std::collections::HashMap;

use crate::AuthError;

/// Failures are forgotten after this long without a new one.
const WINDOW_SECS: u64 = 15 * 60;
/// How long a lockout lasts.
pub const LOCKOUT_SECS: u64 = 15 * 60;
/// The longest wait between two tries during the slow-down.
const MAX_WAIT_SECS: u64 = 60;
/// At most this many keys are tracked; the oldest are dropped first.
const MAX_KEYS: usize = 10_000;

/// When a kind of key slows down and locks.
#[derive(Clone, Copy)]
pub(crate) struct Rule {
    /// Failures without any wait.
    pub free: u32,
    /// Failures that start a lockout.
    pub lock: u32,
}

/// One username: three free tries, a growing wait, locked at the tenth failure.
pub(crate) const PER_USER: Rule = Rule { free: 3, lock: 10 };
/// One address may try a few usernames before it slows down.
pub(crate) const PER_SOURCE: Rule = Rule { free: 10, lock: 30 };

#[derive(Default, Clone)]
struct Counter {
    failures: u32,
    last: u64,
    locked_until: u64,
}

#[derive(Default)]
pub(crate) struct Limits {
    counters: HashMap<String, Counter>,
}

impl Limits {
    /// Refuse a try now: locked, or too soon after the last failure.
    pub fn check(&self, key: &str, rule: Rule, now: u64) -> Result<(), AuthError> {
        let Some(c) = self.counters.get(key) else {
            return Ok(());
        };
        if c.locked_until > now {
            return Err(AuthError::Locked {
                wait_secs: c.locked_until - now,
            });
        }
        if now.saturating_sub(c.last) > WINDOW_SECS || c.failures <= rule.free {
            return Ok(());
        }
        let wait = 2u64
            .saturating_pow(c.failures - rule.free)
            .min(MAX_WAIT_SECS);
        let ready = c.last + wait;
        if now < ready {
            return Err(AuthError::SlowDown {
                wait_secs: ready - now,
            });
        }
        Ok(())
    }

    /// Count a failure. True when it starts a lockout.
    pub fn fail(&mut self, key: &str, rule: Rule, now: u64) -> bool {
        if self.counters.len() >= MAX_KEYS && !self.counters.contains_key(key) {
            self.prune(now);
        }
        let c = self.counters.entry(key.to_string()).or_default();
        if now.saturating_sub(c.last) > WINDOW_SECS {
            c.failures = 0;
        }
        c.failures += 1;
        c.last = now;
        if c.failures >= rule.lock {
            c.failures = 0;
            c.locked_until = now + LOCKOUT_SECS;
            return true;
        }
        false
    }

    /// A success clears the key.
    pub fn clear(&mut self, key: &str) {
        self.counters.remove(key);
    }

    fn prune(&mut self, now: u64) {
        self.counters
            .retain(|_, c| c.locked_until > now || now.saturating_sub(c.last) <= WINDOW_SECS);
        if self.counters.len() >= MAX_KEYS {
            // Still full: drop the oldest half.
            let mut lasts: Vec<u64> = self.counters.values().map(|c| c.last).collect();
            lasts.sort_unstable();
            let cut = lasts[lasts.len() / 2];
            self.counters
                .retain(|_, c| c.last > cut || c.locked_until > now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slows_down_then_locks() {
        let mut l = Limits::default();
        let rule = Rule { free: 3, lock: 6 };
        for t in 0..3 {
            assert!(l.check("u:a", rule, t).is_ok());
            assert!(!l.fail("u:a", rule, t));
        }
        // Fourth failure: from now on a wait (2 s after it).
        assert!(!l.fail("u:a", rule, 3));
        assert!(matches!(
            l.check("u:a", rule, 3),
            Err(AuthError::SlowDown { wait_secs: 2 })
        ));
        assert!(l.check("u:a", rule, 5).is_ok());
        assert!(!l.fail("u:a", rule, 5));
        assert!(l.check("u:a", rule, 8).is_err());
        assert!(l.check("u:a", rule, 9).is_ok());
        assert!(l.fail("u:a", rule, 9), "sixth failure locks");
        assert!(matches!(
            l.check("u:a", rule, 10),
            Err(AuthError::Locked { .. })
        ));
        assert!(l.check("u:a", rule, 9 + LOCKOUT_SECS).is_ok());
        // Other keys are unaffected; a success clears.
        assert!(l.check("u:b", rule, 10).is_ok());
        l.clear("u:a");
        assert!(l.check("u:a", rule, 10).is_ok());
    }

    #[test]
    fn old_failures_are_forgotten() {
        let mut l = Limits::default();
        let rule = Rule { free: 1, lock: 3 };
        l.fail("k", rule, 0);
        l.fail("k", rule, 1);
        assert!(!l.fail("k", rule, 1 + WINDOW_SECS + 1), "window restarted");
    }
}
