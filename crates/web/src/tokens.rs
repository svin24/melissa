//! Design tokens: `design/tokens.json` (the export of the Penpot design library) and the
//! hand-written `:root` of `assets/melissa.css` must agree (docs/design/COMPONENT-SPEC.md
//! section 1, docs/DEV-ENV.md "Design tokens").
//!
//! A Penpot token `a.b.c` is the CSS variable `--a-b-c`. The test below fails when a token is
//! missing on either side or resolves to a different value in either theme. The stylesheet
//! may define a few names the design library doesn't have: the old names kept as aliases and
//! some local layout values ([`CSS_ONLY`]).

/// The token file, as exported from Penpot (or written by `scripts/tokens/tokens_gen.py`).
pub const TOKENS_JSON: &str = include_str!("../design/tokens.json");

/// Names in the stylesheet's `:root` that the design library doesn't have.
pub const CSS_ONLY: &[&str] = &[
    // A timing curve (Penpot has no token type for it).
    "--motion-ease-out",
    // Local layout values of one component, not part of the design library.
    "--limit-card-min-width",
    "--limit-meter-height",
    "--limit-mark-size",
    "--app-icon-size",
    "--app-icon-size-lg",
    "--app-icon-bg",
    "--app-row-person-width",
    "--app-row-status-width",
    "--app-fact-label-width",
    "--app-log-max-height",
    // The names rules used before the layers, kept as aliases of the semantic tokens until
    // every rule reads those (2026-11-22; listed in docs/DECISIONS.md).
    "--ground",
    "--surface",
    "--ink",
    "--muted",
    "--border",
    "--honey",
    "--honey-text",
    "--honey-soft",
    "--action",
    "--action-ink",
    "--caution",
    "--caution-soft",
    "--caution-ink",
    "--ok",
    "--idle",
    "--bubble",
    "--bubble-ink",
    "--focus",
    "--backdrop",
    "--shadow-float",
    "--icon-green",
    "--icon-teal",
    "--icon-blue",
    "--icon-violet",
    "--icon-rose",
    "--icon-orange",
    "--icon-lime",
    "--icon-slate",
    "--radius",
    "--font",
    "--font-head",
    "--mono",
];

/// First segments of the semantic layer (everything else outside `ref.*` is a component).
const SEMANTIC_ROOTS: &[&str] = &[
    "color", "space", "radius", "size", "font", "line", "border", "focus", "motion",
];

/// The layer a token belongs to, from its CSS name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Primitive,
    Semantic,
    Component,
}

pub fn layer(css_name: &str) -> Layer {
    let first = css_name
        .trim_start_matches("--")
        .split('-')
        .next()
        .unwrap_or("");
    if first == "ref" {
        Layer::Primitive
    } else if SEMANTIC_ROOTS.contains(&first) {
        Layer::Semantic
    } else {
        Layer::Component
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    const CSS: &str = include_str!("../assets/melissa.css");

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Theme {
        Light,
        Dark,
    }
    const THEMES: [Theme; 2] = [Theme::Light, Theme::Dark];

    fn strip_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(at) = rest.find("/*") {
            out.push_str(&rest[..at]);
            rest = rest[at..]
                .find("*/")
                .map_or("", |end| &rest[at + end + 2..]);
        }
        out.push_str(rest);
        out
    }

    /// The custom properties of each `:root` block (the first is the token block; later ones
    /// are themes and media overrides), in order.
    fn root_blocks() -> Vec<Vec<(String, String)>> {
        let css = strip_comments(CSS);
        let mut blocks = Vec::new();
        let mut rest = css.as_str();
        while let Some(at) = rest.find(":root") {
            let after = &rest[at..];
            let (Some(open), Some(close)) = (after.find('{'), after.find('}')) else {
                break;
            };
            let mut decls = Vec::new();
            for decl in after[open + 1..close].split(';') {
                if let Some((name, value)) = decl.split_once(':') {
                    let name = name.trim();
                    if name.starts_with("--") {
                        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
                        decls.push((name.to_owned(), value));
                    }
                }
            }
            blocks.push(decls);
            rest = &after[close..];
        }
        blocks
    }

    fn css_tokens() -> BTreeMap<String, String> {
        let blocks = root_blocks();
        let first = blocks.first().expect("melissa.css has a :root block");
        let mut out = BTreeMap::new();
        for (n, v) in first {
            assert!(
                out.insert(n.clone(), v.clone()).is_none(),
                "{n} is declared twice in :root"
            );
        }
        out
    }

    /// `(type, value)` of every token in a set, by CSS name.
    type Set = BTreeMap<String, (String, String)>;

    fn json_sets() -> BTreeMap<String, Set> {
        fn walk(node: &serde_json::Value, path: &mut Vec<String>, out: &mut Set) {
            let Some(obj) = node.as_object() else { return };
            if let Some(v) = obj.get("$value") {
                let typ = obj.get("$type").and_then(|t| t.as_str()).unwrap_or("");
                let value = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.insert(format!("--{}", path.join("-")), (typ.to_owned(), value));
                return;
            }
            for (k, child) in obj {
                path.push(k.clone());
                walk(child, path, out);
                path.pop();
            }
        }
        let doc: serde_json::Value = serde_json::from_str(TOKENS_JSON).expect("tokens.json");
        let mut sets = BTreeMap::new();
        for name in ["core", "theme/light", "theme/dark"] {
            let mut set = Set::new();
            walk(&doc[name], &mut Vec::new(), &mut set);
            assert!(!set.is_empty(), "tokens.json has no set {name}");
            sets.insert(name.to_owned(), set);
        }
        sets
    }

    /// Every token of one theme: `core` plus that theme's set.
    fn json_theme(sets: &BTreeMap<String, Set>, theme: Theme) -> Set {
        let mut all = sets["core"].clone();
        let name = match theme {
            Theme::Light => "theme/light",
            Theme::Dark => "theme/dark",
        };
        for (k, v) in &sets[name] {
            assert!(
                all.insert(k.clone(), v.clone()).is_none(),
                "{k} is in core and {name}"
            );
        }
        all
    }

    fn resolve_json(tokens: &Set, name: &str, depth: u8) -> Option<String> {
        let (_, value) = tokens.get(name)?;
        let value = value.trim();
        if depth < 16 && value.starts_with('{') && value.ends_with('}') {
            return resolve_json(
                tokens,
                &format!("--{}", value[1..value.len() - 1].replace('.', "-")),
                depth + 1,
            );
        }
        Some(value.to_owned())
    }

    /// The arguments of `fn(…)` split at top-level commas.
    fn args<'a>(value: &'a str, func: &str) -> Option<Vec<&'a str>> {
        let inner = value
            .strip_prefix(func)?
            .strip_prefix('(')?
            .strip_suffix(')')?;
        let (mut depth, mut start, mut out) = (0i32, 0, Vec::new());
        for (i, ch) in inner.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(inner[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(inner[start..].trim());
        Some(out)
    }

    fn resolve_css(
        tokens: &BTreeMap<String, String>,
        value: &str,
        theme: Theme,
        depth: u8,
    ) -> String {
        let value = value.trim();
        if depth > 16 {
            return value.to_owned();
        }
        if let Some(a) = args(value, "light-dark").filter(|a| a.len() == 2) {
            let side = if theme == Theme::Light { a[0] } else { a[1] };
            return resolve_css(tokens, side, theme, depth + 1);
        }
        if let Some(a) = args(value, "var").filter(|a| a.len() == 1) {
            if let Some(v) = tokens.get(a[0]) {
                return resolve_css(tokens, v, theme, depth + 1);
            }
        }
        value.to_owned()
    }

    fn hex(s: &str) -> Option<[u8; 4]> {
        let h = s.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        let nib = |i: usize| u8::from_str_radix(h.get(i..=i)?, 16).ok().map(|n| n * 17);
        match h.len() {
            3 => Some([nib(0)?, nib(1)?, nib(2)?, 255]),
            6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
            8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
            _ => None,
        }
    }

    /// `rgb(10 14 20 / 0.45)`.
    fn rgb(s: &str) -> Option<[u8; 4]> {
        let a = args(s, "rgb")?;
        let (chan, alpha) = match a[..] {
            [one] => one
                .split_once('/')
                .map_or((one, None), |(c, a)| (c, Some(a))),
            _ => return None,
        };
        let c: Vec<u8> = chan
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        let alpha = alpha.map_or(Some(1.0), |a| a.trim().parse::<f64>().ok())?;
        let [r, g, b] = c[..] else { return None };
        Some([r, g, b, (alpha * 255.0).round() as u8])
    }

    /// One comparable spelling: colours as `#rrggbbaa`, lengths and times as px/ms numbers,
    /// font stacks as their first family.
    fn normal(typ: &str, value: &str) -> String {
        let v = value.trim();
        if typ == "color" {
            if let Some([r, g, b, a]) = hex(v).or_else(|| rgb(v)) {
                return format!("#{r:02x}{g:02x}{b:02x}{a:02x}");
            }
            return v.to_lowercase();
        }
        if typ == "fontFamilies" {
            return v
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .to_owned();
        }
        let num = |s: &str, unit: &str, scale: f64| {
            s.strip_suffix(unit)
                .and_then(|n| n.trim().parse::<f64>().ok())
                .map(|n| n * scale)
        };
        let n = num(v, "rem", 16.0)
            .or_else(|| num(v, "px", 1.0))
            .or_else(|| num(v, "ms", 1.0))
            .or_else(|| v.parse::<f64>().ok());
        match n {
            Some(n) => format!("{:.2}", n),
            None => v.to_owned(),
        }
    }

    #[test]
    fn tokens_json_and_the_stylesheet_agree() {
        let css = css_tokens();
        let sets = json_sets();
        let mut problems = Vec::new();

        let in_json: BTreeSet<&String> = sets.values().flat_map(|s| s.keys()).collect();
        for name in &in_json {
            if !css.contains_key(*name) {
                problems.push(format!(
                    "{name}: in tokens.json, missing in melissa.css :root"
                ));
            }
        }
        for name in css.keys() {
            let only_css = CSS_ONLY.contains(&name.as_str());
            if !in_json.contains(name) && !only_css {
                problems.push(format!(
                    "{name}: in melissa.css :root, missing in tokens.json (add it to the design \
                     library, or to tokens::CSS_ONLY if it is a local value)"
                ));
            }
            if in_json.contains(name) && only_css {
                problems.push(format!("{name}: in tokens.json and in tokens::CSS_ONLY"));
            }
        }
        for name in CSS_ONLY {
            if !css.contains_key(*name) {
                problems.push(format!(
                    "{name}: in tokens::CSS_ONLY, missing in melissa.css"
                ));
            }
        }

        for theme in THEMES {
            let tokens = json_theme(&sets, theme);
            for (name, (typ, _)) in &tokens {
                let Some(css_value) = css.get(name) else {
                    continue;
                };
                let want = resolve_json(&tokens, name, 0).unwrap_or_default();
                let got = resolve_css(&css, css_value, theme, 0);
                if normal(typ, &want) != normal(typ, &got) {
                    problems.push(format!(
                        "{name} ({theme:?}): tokens.json {want}, melissa.css {got}"
                    ));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "tokens.json and melissa.css disagree (docs/DEV-ENV.md, Design tokens):\n  {}",
            problems.join("\n  ")
        );
    }

    #[test]
    fn later_root_blocks_only_override_tokens() {
        let blocks = root_blocks();
        let first: BTreeSet<&String> = blocks[0].iter().map(|(n, _)| n).collect();
        for block in &blocks[1..] {
            for (name, _) in block {
                assert!(
                    first.contains(name),
                    "{name} is only set in a later :root block"
                );
            }
        }
    }

    #[test]
    fn layers_read_only_the_layer_below() {
        let css = css_tokens();
        let refs = |value: &str| -> Vec<String> {
            value
                .split("var(")
                .skip(1)
                .filter_map(|v| v.split([')', ',']).next())
                .map(|v| v.trim().to_owned())
                .collect()
        };
        for (name, value) in &css {
            for r in refs(value) {
                let ok = match (layer(name), layer(&r)) {
                    _ if !css.contains_key(&r) => false,
                    (Layer::Primitive, _) => false,
                    (Layer::Semantic, Layer::Primitive | Layer::Semantic) => true,
                    (Layer::Component, Layer::Semantic | Layer::Component) => true,
                    _ => false,
                };
                assert!(ok, "{name} reads {r}");
            }
        }
        // Colours are written once, for both schemes.
        for (name, value) in &css {
            if name.starts_with("--color-") {
                assert!(value.starts_with("light-dark("), "{name}: {value}");
            }
        }
    }

    #[test]
    fn the_checker_reads_both_spellings() {
        assert_eq!(
            normal("color", "#0a0e1473"),
            normal("color", "rgb(10 14 20 / 0.45)")
        );
        assert_eq!(normal("spacing", "4"), normal("spacing", "0.25rem"));
        assert_eq!(normal("number", "150"), normal("number", "150ms"));
        assert_eq!(
            normal("fontFamilies", "Bricolage Grotesque"),
            normal("fontFamilies", "\"Bricolage Grotesque\", system-ui")
        );
        assert_ne!(normal("color", "#ffffff"), normal("color", "#fffffe"));
    }
}
