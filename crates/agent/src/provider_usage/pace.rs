//! The pace marker: where usage would be now if the window were used evenly. A window of
//! length `L` that resets at `R` started at `R - L`; at `now`, `(now - (R - L)) / L` of it has
//! passed, so an even pace has used that share. Ahead of pace means the window may run out
//! before it resets.

use serde::{Deserialize, Serialize};

/// How far apart (percentage points) used and expected must be to count as ahead or under.
pub const ON_PACE_BAND: f64 = 5.0;

/// The pace of one window at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pace {
    /// Share of the window that has passed, in percent (0–100): the used share an even pace
    /// would show now.
    pub expected_pct: f64,
    /// `used_pct - expected_pct`: positive is ahead of pace.
    pub delta_pct: f64,
    pub verdict: Verdict,
}

/// Ahead of, on, or under pace (within [`ON_PACE_BAND`] is on pace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Ahead,
    On,
    Under,
}

/// The pace of a window `window_secs` long that resets at `resets_at` and is `used_pct` used,
/// at `now` (all Unix seconds). `None` for a zero length or a non-finite share.
pub fn pace(used_pct: f64, resets_at: u64, window_secs: u64, now: u64) -> Option<Pace> {
    if window_secs == 0 || !used_pct.is_finite() {
        return None;
    }
    let left = resets_at.saturating_sub(now).min(window_secs);
    let passed = (window_secs - left) as f64 / window_secs as f64;
    let expected_pct = (passed * 100.0).clamp(0.0, 100.0);
    let delta_pct = used_pct - expected_pct;
    let verdict = if delta_pct > ON_PACE_BAND {
        Verdict::Ahead
    } else if delta_pct < -ON_PACE_BAND {
        Verdict::Under
    } else {
        Verdict::On
    };
    Some(Pace {
        expected_pct,
        delta_pct,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test]
    fn halfway_through_a_window_expects_half_used() {
        let p = pace(50.0, 1_000 + 5 * H / 2, 5 * H, 1_000).unwrap();
        assert!((p.expected_pct - 50.0).abs() < 1e-9);
        assert!(p.delta_pct.abs() < 1e-9);
        assert_eq!(p.verdict, Verdict::On);
    }

    #[test]
    fn using_more_than_the_time_passed_is_ahead() {
        // A week, one day in, 40 % used: expected ~14.3 %.
        let p = pace(40.0, 6 * 24 * H, 7 * 24 * H, 0).unwrap();
        assert!((p.expected_pct - 100.0 / 7.0).abs() < 1e-9);
        assert_eq!(p.verdict, Verdict::Ahead);
        let p = pace(1.0, 6 * 24 * H, 7 * 24 * H, 0).unwrap();
        assert_eq!(p.verdict, Verdict::Under);
    }

    #[test]
    fn past_the_reset_or_before_the_start_stays_within_0_and_100() {
        // The reset time has passed: the whole window has passed.
        let p = pace(120.0, 100, 5 * H, 200).unwrap();
        assert!((p.expected_pct - 100.0).abs() < 1e-9);
        // A reset further away than the window is long (clock skew): nothing passed yet.
        let p = pace(0.0, 10 * H, 5 * H, 0).unwrap();
        assert!(p.expected_pct.abs() < 1e-9);
    }

    #[test]
    fn no_pace_without_a_length_or_a_number() {
        assert!(pace(10.0, 100, 0, 0).is_none());
        assert!(pace(f64::NAN, 100, H, 0).is_none());
    }
}
