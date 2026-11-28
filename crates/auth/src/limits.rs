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
/// Above this many keys, stale ones and those that may be dropped (addresses) go first.
const MAX_KEYS: usize = 10_000;
/// The hard cap. Username counters with failures count against it only; when even those fill
/// it, the ones with the fewest failures go first, so a username under attack (many
/// failures) stays counted while made-up usernames (one failure each) make room.
const HARD_MAX_KEYS: usize = 10 * MAX_KEYS;

/// When a kind of key slows down and locks.
#[derive(Clone, Copy)]
pub(crate) struct Rule {
    /// Failures without any wait.
    pub free: u32,
    /// Failures that start a lockout.
    pub lock: u32,
    /// Keep the failures even when the table is full: someone may be guessing this key's
    /// password, and dropping the counter would give them fresh tries (security review
    /// 2026-11-28: flooding the table with made-up keys must not reset a username).
    pub keep: bool,
}

/// One username: three free tries, a growing wait, locked at the tenth failure.
pub(crate) const PER_USER: Rule = Rule {
    free: 3,
    lock: 10,
    keep: true,
};
/// One address may try a few usernames before it slows down.
pub(crate) const PER_SOURCE: Rule = Rule {
    free: 10,
    lock: 30,
    keep: false,
};

#[derive(Default, Clone)]
struct Counter {
    failures: u32,
    last: u64,
    locked_until: u64,
    /// [`Rule::keep`].
    keep: bool,
}

#[derive(Default)]
pub(crate) struct Limits {
    counters: HashMap<String, Counter>,
    /// The size at which the next new key prunes (0: [`MAX_KEYS`]). Above the soft cap the
    /// table may hold kept usernames that can't go: prune again only after some growth, so a
    /// flood doesn't scan the whole table at every failure.
    prune_at: usize,
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
        if self.counters.len() >= self.prune_at.max(MAX_KEYS) && !self.counters.contains_key(key) {
            self.prune(now);
            self.prune_at = (self.counters.len() + MAX_KEYS / 10).min(HARD_MAX_KEYS);
        }
        let c = self.counters.entry(key.to_string()).or_default();
        c.keep = rule.keep;
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

    /// Make room. Never dropped: a running lockout. Dropped only at the hard cap: a kept key
    /// (a username) with failures in the window, fewest failures first.
    fn prune(&mut self, now: u64) {
        let live = |c: &Counter| c.locked_until > now || now.saturating_sub(c.last) <= WINDOW_SECS;
        self.counters.retain(|_, c| live(c));
        let guarded = |c: &Counter| c.locked_until > now || (c.keep && c.failures > 0);
        if self.counters.len() >= MAX_KEYS {
            // Still full: the oldest half of what may go (addresses, counters at zero).
            let mut lasts: Vec<u64> = self
                .counters
                .values()
                .filter(|c| !guarded(c))
                .map(|c| c.last)
                .collect();
            if !lasts.is_empty() {
                lasts.sort_unstable();
                let cut = lasts[lasts.len() / 2];
                self.counters.retain(|_, c| guarded(c) || c.last > cut);
            }
        }
        if self.counters.len() >= HARD_MAX_KEYS {
            // Full of usernames with failures: the half with the fewest failures (then the
            // oldest) goes; a username being guessed has the most and stays.
            let mut order: Vec<(u32, u64)> = self
                .counters
                .values()
                .filter(|c| c.locked_until <= now)
                .map(|c| (c.failures, c.last))
                .collect();
            if !order.is_empty() {
                order.sort_unstable();
                let cut = order[order.len() / 2];
                self.counters
                    .retain(|_, c| c.locked_until > now || (c.failures, c.last) > cut);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slows_down_then_locks() {
        let mut l = Limits::default();
        let rule = Rule {
            free: 3,
            lock: 6,
            keep: true,
        };
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
        let rule = Rule {
            free: 1,
            lock: 3,
            keep: true,
        };
        l.fail("k", rule, 0);
        l.fail("k", rule, 1);
        assert!(!l.fail("k", rule, 1 + WINDOW_SECS + 1), "window restarted");
    }

    /// Security review 2026-11-28 (S1): made-up usernames and addresses flooding the table
    /// never reset the owner's username, which keeps slowing down and locks as before.
    #[test]
    fn a_flood_never_resets_a_username_being_guessed() {
        let mut l = Limits::default();
        for t in 0..5 {
            l.fail("u:owner", PER_USER, t);
        }
        let slowed = |l: &Limits, now| l.check("u:owner", PER_USER, now).is_err();
        assert!(slowed(&l, 5), "the fifth failure means a wait");
        // Twice the soft cap of addresses, then made-up usernames up to the hard cap and
        // beyond, all within the window.
        for i in 0..2 * MAX_KEYS {
            l.fail(&format!("ip:{i}"), PER_SOURCE, 6);
        }
        assert!(slowed(&l, 6), "addresses never push a username out");
        for i in 0..HARD_MAX_KEYS + MAX_KEYS {
            l.fail(&format!("u:x{i}"), PER_USER, 7);
        }
        assert!(l.counters.len() <= HARD_MAX_KEYS, "still bounded");
        assert_eq!(l.counters["u:owner"].failures, 5, "kept with its failures");
        // Five more failures lock it, as without the flood.
        let mut now = 100;
        let mut locked = false;
        for _ in 0..5 {
            now += MAX_WAIT_SECS;
            locked = l.fail("u:owner", PER_USER, now);
        }
        assert!(locked, "the tenth failure locks");
        assert!(matches!(
            l.check("u:owner", PER_USER, now + 1),
            Err(AuthError::Locked { .. })
        ));
    }

    #[test]
    fn stale_and_address_counters_make_room_first() {
        let mut l = Limits::default();
        l.fail("u:a", PER_USER, 0);
        for i in 0..MAX_KEYS {
            l.fail(&format!("ip:{i}"), PER_SOURCE, 1);
        }
        assert!(l.counters.len() < MAX_KEYS, "the oldest addresses went");
        assert!(l.counters.contains_key("u:a"));
        // Past the window everything not locked is forgotten at the next prune.
        let later = 2 + WINDOW_SECS + 1;
        for i in 0..MAX_KEYS {
            l.fail(&format!("ip:n{i}"), PER_SOURCE, later);
        }
        assert!(!l.counters.contains_key("u:a"), "stale");
    }
}
