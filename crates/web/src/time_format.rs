//! Times of day for people: 24-hour ("15:00") or 12-hour ("3:00 PM"), as the owner chose in
//! Settings → About → This device (`Prefs::time_format`). "Follow browser" (the default) reads the
//! request's `Accept-Language`: the languages that write the 12-hour clock get it, everyone else
//! the 24-hour one. Every page that shows a time of day can use [`Hours`]; the Updates page does.

use axum::http::{header, HeaderMap};
use melissa_settings::prefs::TimeFormat;

/// The clock a page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hours {
    H24,
    H12,
}

/// Languages (and language-region pairs) whose usual clock is the 12-hour one (CLDR's `h a`
/// time formats). A region on its own wins over its language ("en-GB" is 24-hour).
const TWELVE: [&str; 20] = [
    "en", "en-us", "en-ca", "en-au", "en-nz", "en-in", "en-ph", "en-pk", "es-us", "es-mx", "hi",
    "bn", "ur", "ar", "ko", "fil", "zh-tw", "zh-hk", "mr", "ta",
];
/// English regions with the 24-hour clock.
const TWENTY_FOUR_EN: [&str; 6] = ["en-gb", "en-ie", "en-za", "en-150", "en-de", "en-nl"];

impl Hours {
    /// The clock for `pref`, with `headers` (the request) for "Follow browser".
    pub fn pick(pref: TimeFormat, headers: &HeaderMap) -> Self {
        match pref {
            TimeFormat::H24 => Self::H24,
            TimeFormat::H12 => Self::H12,
            TimeFormat::Browser => Self::from_language(
                headers
                    .get(header::ACCEPT_LANGUAGE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            ),
        }
    }

    /// From an `Accept-Language` value: its first language decides (24-hour when there is none).
    pub fn from_language(accept: &str) -> Self {
        let first = accept
            .split(',')
            .next()
            .and_then(|l| l.split(';').next())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        if first.is_empty() || first == "*" {
            return Self::H24;
        }
        if TWENTY_FOUR_EN.contains(&first.as_str()) {
            return Self::H24;
        }
        if TWELVE.contains(&first.as_str()) {
            return Self::H12;
        }
        // Another region: the language's own clock, except where it varies by region (Spanish,
        // Chinese), which keeps the 24-hour one unless listed.
        let lang = first.split('-').next().unwrap_or("");
        if TWELVE.contains(&lang) && lang != "es" && lang != "zh" {
            Self::H12
        } else {
            Self::H24
        }
    }

    /// A time of day: `03:00` or `3:00 AM`.
    pub fn time(self, hour: u8, minute: u8) -> String {
        match self {
            Self::H24 => format!("{hour:02}:{minute:02}"),
            Self::H12 => {
                let h = match hour % 12 {
                    0 => 12,
                    h => h,
                };
                let half = if hour < 12 { "AM" } else { "PM" };
                format!("{h}:{minute:02}\u{a0}{half}")
            }
        }
    }

    /// A whole hour: `03:00` or `3:00 AM`.
    pub fn hour(self, hour: u8) -> String {
        self.time(hour, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_browsers_language() {
        for (accept, want) in [
            ("en-US,en;q=0.9", Hours::H12),
            ("en", Hours::H12),
            ("en-GB,en;q=0.8", Hours::H24),
            ("de-DE,de;q=0.9,en;q=0.5", Hours::H24),
            ("fr", Hours::H24),
            ("ko-KR", Hours::H12),
            ("zh-CN", Hours::H24),
            ("zh-TW", Hours::H12),
            ("es-ES", Hours::H24),
            ("es-MX", Hours::H12),
            ("en_AU", Hours::H12),
            ("", Hours::H24),
            ("*", Hours::H24),
        ] {
            assert_eq!(Hours::from_language(accept), want, "{accept}");
        }
    }

    #[test]
    fn the_owners_choice_wins() {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_LANGUAGE, "en-US".parse().unwrap());
        assert_eq!(Hours::pick(TimeFormat::Browser, &h), Hours::H12);
        assert_eq!(Hours::pick(TimeFormat::H24, &h), Hours::H24);
        assert_eq!(Hours::pick(TimeFormat::H12, &HeaderMap::new()), Hours::H12);
    }

    #[test]
    fn formats_hours() {
        assert_eq!(Hours::H24.hour(3), "03:00");
        assert_eq!(Hours::H24.time(15, 7), "15:07");
        assert_eq!(Hours::H12.hour(0), "12:00\u{a0}AM");
        assert_eq!(Hours::H12.hour(12), "12:00\u{a0}PM");
        assert_eq!(Hours::H12.time(15, 7), "3:07\u{a0}PM");
    }
}
