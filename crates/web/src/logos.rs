//! Provider logos: small single-colour marks that say which agent a model belongs to (the
//! model picker, Settings → Providers, Add provider). Everything else gets Melissa's letter
//! mark ([`crate::components::provider_mark`]).
//!
//! The SVG files live in `assets/logos/` (vendored from icon sets, see
//! `assets/README.md` and ACKNOWLEDGMENTS.md) and are compiled in. They are never sent as
//! markup: [`parse`] reads each with a strict allowlist (one `<svg>` with `xmlns` and
//! `viewBox`, an optional `<title>`, `<path>`s with `d` and `fill-rule` only) and the
//! component draws the paths itself, escaped like any other text. A file that doesn't pass
//! is left out (letter mark), and a test fails.
//!
//! Which brands get a logo is a decision about their logo guidelines (docs/DECISIONS.md,
//! 2026-11-20): only brands whose public terms allow showing their mark to identify the
//! product, or that publish no terms, and only as a plain one-colour mark. Claude, Codex /
//! OpenAI, Gemini, GitHub Copilot, Kimi and Mistral keep the letter mark.

use std::{collections::BTreeMap, sync::LazyLock};

/// One drawn path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoPath {
    pub d: String,
    pub evenodd: bool,
}

/// A parsed logo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logo {
    pub title: String,
    pub view_box: String,
    pub paths: Vec<LogoPath>,
}

/// Why a file isn't a logo Melissa draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoError(String);

impl std::fmt::Display for LogoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(msg: impl Into<String>) -> LogoError {
    LogoError(msg.into())
}

/// The vendored files, by key (the file name without `.svg`).
const FILES: [(&str, &str); 5] = [
    ("cline", include_str!("../assets/logos/cline.svg")),
    ("cursor", include_str!("../assets/logos/cursor.svg")),
    ("goose", include_str!("../assets/logos/goose.svg")),
    ("grok", include_str!("../assets/logos/grok.svg")),
    ("opencode", include_str!("../assets/logos/opencode.svg")),
];

static LOGOS: LazyLock<BTreeMap<&'static str, Logo>> = LazyLock::new(|| {
    FILES
        .iter()
        .filter_map(|(key, svg)| match parse(svg) {
            Ok(logo) => Some((*key, logo)),
            Err(e) => {
                tracing::error!(logo = key, "logo left out: {e}");
                None
            }
        })
        .collect()
});

/// The logo key for a backend name (`grok-acp`), provider id (`opencode`) or ACP registry id
/// (`cursor`). None: use the letter mark.
pub fn key_for(name: &str) -> Option<&'static str> {
    let base = name
        .strip_suffix("-acp")
        .or_else(|| name.strip_suffix("-session"))
        .unwrap_or(name);
    Some(match base {
        "grok" | "grok-build" => "grok",
        "opencode" | "opencode-v2" => "opencode",
        "cursor" | "cursor-agent" => "cursor",
        "cline" => "cline",
        "goose" => "goose",
        _ => return None,
    })
}

/// The logo for a backend, provider or registry agent, if Melissa ships one.
pub fn logo(name: &str) -> Option<&'static Logo> {
    key_for(name).and_then(|k| LOGOS.get(k))
}

// --- The allowlist parser ---------------------------------------------------------------------

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const MAX_FILE: usize = 16 * 1024;

struct Tag<'a> {
    name: &'a str,
    attrs: Vec<(&'a str, &'a str)>,
    closing: bool,
    self_closing: bool,
}

/// Split `s` into tags and the text between them. Comments, processing instructions,
/// doctypes, CDATA and entities are refused (`<!`, `<?`, `&`).
fn tokens(s: &str) -> Result<Vec<Result<Tag<'_>, &str>>, LogoError> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let end = rest.find('>').ok_or_else(|| bad("unclosed tag"))?;
                out.push(Ok(tag(&rest[1..end])?));
                rest = &rest[end + 1..];
            }
            Some(i) => {
                out.push(Err(&rest[..i]));
                rest = &rest[i..];
            }
            None => {
                out.push(Err(rest));
                rest = "";
            }
        }
    }
    Ok(out)
}

fn tag(inner: &str) -> Result<Tag<'_>, LogoError> {
    if inner.starts_with('!') || inner.starts_with('?') {
        return Err(bad(
            "comments, doctypes and processing instructions are not allowed",
        ));
    }
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, inner),
    };
    let (self_closing, inner) = match inner.strip_suffix('/') {
        Some(r) => (true, r),
        None => (false, inner),
    };
    let inner = inner.trim();
    let name_end = inner
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(bad("bad element name"));
    }
    let mut attrs = Vec::new();
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest
            .find('=')
            .ok_or_else(|| bad("attribute without a value"))?;
        let key = rest[..eq].trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(bad("bad attribute name"));
        }
        let after = rest[eq + 1..].trim_start();
        let value_part = after
            .strip_prefix('"')
            .ok_or_else(|| bad("attribute values must be in double quotes"))?;
        let close = value_part
            .find('"')
            .ok_or_else(|| bad("unclosed attribute value"))?;
        attrs.push((key, &value_part[..close]));
        rest = value_part[close + 1..].trim_start();
    }
    if closing && (!attrs.is_empty() || self_closing) {
        return Err(bad("bad closing tag"));
    }
    Ok(Tag {
        name,
        attrs,
        closing,
        self_closing,
    })
}

fn view_box_ok(v: &str) -> bool {
    let nums: Vec<&str> = v.split_ascii_whitespace().collect();
    nums.len() == 4
        && nums
            .iter()
            .all(|n| n.parse::<f64>().is_ok_and(f64::is_finite))
}

fn path_data_ok(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 8192
        && d.bytes().all(|b| {
            b.is_ascii_digit()
                || b"MmLlHhVvCcSsQqTtAaZz .,-+eE".contains(&b)
                || b.is_ascii_whitespace()
        })
}

/// Read a logo file with the allowlist described in the module docs.
pub fn parse(svg: &str) -> Result<Logo, LogoError> {
    if svg.len() > MAX_FILE {
        return Err(bad("file too large"));
    }
    if svg.contains('&') {
        return Err(bad("entities are not allowed"));
    }
    let mut it = tokens(svg.trim())?.into_iter();
    let Some(Ok(root)) = it.next() else {
        return Err(bad("must start with <svg>"));
    };
    if root.name != "svg" || root.closing || root.self_closing {
        return Err(bad("must start with <svg>"));
    }
    let mut view_box = None;
    for (k, v) in &root.attrs {
        match *k {
            "xmlns" if *v == SVG_NS => {}
            "viewBox" if view_box_ok(v) => view_box = Some(v.to_string()),
            other => return Err(bad(format!("<svg {other}> is not allowed"))),
        }
    }
    let view_box = view_box.ok_or_else(|| bad("<svg> needs a viewBox"))?;
    let mut title = String::new();
    let mut paths = Vec::new();
    let mut ended = false;
    while let Some(tok) = it.next() {
        if ended {
            match tok {
                Err(text) if text.trim().is_empty() => continue,
                _ => return Err(bad("content after </svg>")),
            }
        }
        let t = match tok {
            Err(text) if text.trim().is_empty() => continue,
            Err(_) => return Err(bad("text outside <title>")),
            Ok(t) => t,
        };
        match (t.name, t.closing, t.self_closing) {
            ("svg", true, _) => ended = true,
            ("title", false, false) if t.attrs.is_empty() && title.is_empty() => {
                let text = match it.next() {
                    Some(Err(text)) => text,
                    _ => return Err(bad("empty <title>")),
                };
                match it.next() {
                    Some(Ok(Tag {
                        name: "title",
                        closing: true,
                        ..
                    })) => {}
                    _ => return Err(bad("<title> must hold text only")),
                }
                title = text.trim().to_string();
                if title.is_empty() || title.len() > 60 {
                    return Err(bad("bad <title>"));
                }
            }
            ("path", false, true) => {
                let mut d = None;
                let mut evenodd = false;
                for (k, v) in &t.attrs {
                    match (*k, *v) {
                        ("d", v) if path_data_ok(v) => d = Some(v.to_string()),
                        ("fill-rule", "evenodd") => evenodd = true,
                        ("fill-rule", "nonzero") => {}
                        (other, _) => return Err(bad(format!("<path {other}> is not allowed"))),
                    }
                }
                let d = d.ok_or_else(|| bad("<path> needs d"))?;
                paths.push(LogoPath { d, evenodd });
            }
            (other, _, _) => return Err(bad(format!("<{other}> is not allowed"))),
        }
    }
    if !ended {
        return Err(bad("missing </svg>"));
    }
    if paths.is_empty() || paths.len() > 8 {
        return Err(bad("a logo has 1-8 paths"));
    }
    Ok(Logo {
        title,
        view_box,
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_logo_passes_the_allowlist() {
        for (key, svg) in FILES {
            let logo = parse(svg).unwrap_or_else(|e| panic!("{key}: {e}"));
            assert_eq!(logo.view_box, "0 0 24 24", "{key}");
            assert!(LOGOS.contains_key(key), "{key}");
        }
        // Every file in assets/logos is listed (and only SVGs and their licence are there).
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            match name.strip_suffix(".svg") {
                Some(key) => assert!(FILES.iter().any(|(k, _)| *k == key), "{name} not listed"),
                None => assert!(name.ends_with("LICENSE.txt"), "unexpected {name}"),
            }
        }
    }

    #[test]
    fn the_raw_files_carry_nothing_active() {
        for (key, svg) in FILES {
            let lower = svg.to_ascii_lowercase();
            for word in [
                "<script",
                "foreignobject",
                "href",
                "on",
                "style",
                "url(",
                "<image",
                "<use",
                "<!",
                "&",
            ] {
                let hit = if word == "on" {
                    // Event handlers: ` onload=`, ` onclick=` ...
                    lower
                        .split_ascii_whitespace()
                        .any(|w| w.starts_with("on") && w.contains('='))
                } else {
                    lower.contains(word)
                };
                assert!(!hit, "{key} contains {word}");
            }
        }
    }

    #[test]
    fn active_or_external_content_is_refused() {
        let ok = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><title>X</title><path d="M0 0h24v24H0z"/></svg>"#;
        assert!(parse(ok).is_ok());
        for bad in [
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><script>alert(1)</script><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><foreignObject><div/></foreignObject><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><image href="https://example.com/x.png"/><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><use href="https://example.com/s.svg#a"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" onload="alert(1)"><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" style="background:url(https://example.com)"><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h1z" fill="url(#g)"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h1z" onclick="x()"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="javascript:alert(1)"/></svg>"#,
            r#"<!DOCTYPE svg [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><!-- hi --><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><title>&lt;</title><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 24 24"><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><g><path d="M0 0h1z"/></g></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h1z"/></svg><script/>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d='M0 0h1z'/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1z"/></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"></svg>"#,
        ] {
            assert!(parse(bad).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn names_map_to_logos_or_the_letter_mark() {
        for (name, key) in [
            ("grok-acp", "grok"),
            ("grok", "grok"),
            ("opencode-acp", "opencode"),
            ("cursor", "cursor"),
            ("cursor-agent", "cursor"),
            ("goose", "goose"),
            ("cline", "cline"),
        ] {
            assert_eq!(key_for(name), Some(key), "{name}");
            assert!(logo(name).is_some(), "{name}");
        }
        // Brands whose guidelines ask for permission or the unaltered asset: letter mark.
        for name in [
            "claude",
            "claude-session",
            "claude-acp",
            "codex-acp",
            "gemini",
            "gemini-acp",
            "muse",
            "muse-acp",
            "muse-code",
            "github-copilot-cli",
            "kimi",
            "mistral-vibe",
            "echo",
            "my-agent",
        ] {
            assert!(logo(name).is_none(), "{name}");
        }
    }
}
