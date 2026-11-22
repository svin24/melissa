//! The design tokens defined in `assets/melissa.css` (`:root`), by layer (docs/design/
//! COMPONENT-SPEC.md section 1, 2026-11-22). The stylesheet is hand-written; this list is what a
//! parity check reads: the test below keeps it and the stylesheet in step, and a later test can
//! compare it with the tokens exported from the design library.

/// Layer 1, primitives (`--ref-*`): the raw palette. Only the semantic layer reads them.
pub const PRIMITIVES: &[&str] = &[
    "--ref-white",
    "--ref-black",
    "--ref-neutral-50",
    "--ref-neutral-100",
    "--ref-neutral-150",
    "--ref-neutral-200",
    "--ref-neutral-350",
    "--ref-neutral-400",
    "--ref-neutral-550",
    "--ref-neutral-600",
    "--ref-neutral-750",
    "--ref-neutral-850",
    "--ref-neutral-900",
    "--ref-neutral-950",
    "--ref-neutral-975",
    "--ref-slate-500",
    "--ref-slate-800",
    "--ref-blue-300",
    "--ref-blue-600",
    "--ref-blue-700",
    "--ref-honey-50",
    "--ref-honey-300",
    "--ref-honey-500",
    "--ref-honey-600",
    "--ref-honey-800",
    "--ref-honey-950",
    "--ref-rust-50",
    "--ref-rust-300",
    "--ref-rust-700",
    "--ref-rust-950",
    "--ref-rust-975",
    "--ref-green-300",
    "--ref-green-700",
    "--ref-green-750",
    "--ref-teal-300",
    "--ref-teal-700",
    "--ref-violet-300",
    "--ref-violet-600",
    "--ref-rose-300",
    "--ref-rose-600",
    "--ref-orange-300",
    "--ref-orange-700",
    "--ref-lime-300",
    "--ref-lime-700",
    "--ref-shade-a45",
    "--ref-shade-a60",
    "--ref-black-a18",
    "--ref-black-a45",
];

/// Layer 2, semantic: colours (written once with `light-dark()`), space, radius, sizes, type,
/// lines, focus and motion. What rules read.
pub const SEMANTIC: &[&str] = &[
    "--color-bg-ground",
    "--color-bg-surface",
    "--color-bg-raised",
    "--color-bg-sunken",
    "--color-bg-hover",
    "--color-bg-selected",
    "--color-text-default",
    "--color-text-muted",
    "--color-text-disabled",
    "--color-text-link",
    "--color-border-default",
    "--color-border-strong",
    "--color-action-bg",
    "--color-action-text",
    "--color-focus-ring",
    "--color-caution-bg",
    "--color-caution-text",
    "--color-caution-soft",
    "--color-caution-on",
    "--color-ok-fg",
    "--color-ok-text",
    "--color-attention-fg",
    "--color-attention-text",
    "--color-attention-soft",
    "--color-idle-fg",
    "--color-bubble-bg",
    "--color-bubble-text",
    "--color-backdrop",
    "--color-shadow",
    "--color-avatar-green",
    "--color-avatar-teal",
    "--color-avatar-blue",
    "--color-avatar-violet",
    "--color-avatar-rose",
    "--color-avatar-orange",
    "--color-avatar-lime",
    "--color-avatar-slate",
    "--color-logo",
    "--space-1",
    "--space-2",
    "--space-3",
    "--space-4",
    "--space-5",
    "--space-6",
    "--space-7",
    "--radius-xs",
    "--radius-sm",
    "--radius-md",
    "--radius-lg",
    "--radius-full",
    "--size-control-sm",
    "--size-control-md",
    "--size-control-lg",
    "--size-icon-sm",
    "--size-icon-md",
    "--size-icon-lg",
    "--size-target-touch",
    "--font-family-body",
    "--font-family-head",
    "--font-family-mono",
    "--font-size-xs",
    "--font-size-sm",
    "--font-size-md",
    "--font-size-lg",
    "--font-size-xl",
    "--font-size-control-sm",
    "--font-size-control-md",
    "--font-size-control-lg",
    "--font-weight-regular",
    "--font-weight-strong",
    "--font-weight-heading",
    "--font-weight-brand",
    "--line-height-tight",
    "--line-height-control",
    "--line-height-body",
    "--border-width-hairline",
    "--focus-width",
    "--focus-offset",
    "--motion-duration-fast",
    "--motion-duration-medium",
    "--motion-duration-enter",
    "--motion-ease-out",
];

/// Layer 3, component: one family's knobs (button, field, row, switch), as aliases of semantic
/// tokens.
pub const COMPONENT: &[&str] = &[
    "--button-primary-bg-default",
    "--button-primary-text-default",
    "--button-secondary-bg-default",
    "--button-secondary-text-default",
    "--button-secondary-border-default",
    "--button-secondary-border-hover",
    "--button-danger-bg-default",
    "--button-danger-text-default",
    "--button-link-text-default",
    "--button-md-height",
    "--button-md-padding-x",
    "--button-md-font-size",
    "--button-md-radius",
    "--button-sm-height",
    "--button-sm-padding-x",
    "--button-sm-font-size",
    "--button-sm-radius",
    "--field-bg-default",
    "--field-text-default",
    "--field-border-default",
    "--field-placeholder-default",
    "--field-height",
    "--field-padding-x",
    "--field-radius",
    "--row-bg-hover",
    "--row-bg-current",
    "--chip-height",
    "--chip-font-size",
    "--chip-neutral-bg",
    "--chip-neutral-text",
    "--chip-ok-bg",
    "--chip-ok-text",
    "--chip-attention-bg",
    "--chip-attention-text",
    "--segmented-track-bg",
    "--segmented-selected-bg",
    "--segmented-selected-border",
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
    "--switch-track-bg-off",
    "--switch-track-bg-on",
    "--switch-knob-off",
    "--switch-knob-on",
    "--switch-md-width",
    "--switch-md-height",
];

/// The names rules used before the layers (`--ground`, `--ink`, …), kept as aliases of the
/// semantic tokens until every rule reads those; then removed.
pub const ALIASES: &[&str] = &[
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

/// Every token, all layers.
pub fn all() -> impl Iterator<Item = &'static str> {
    PRIMITIVES
        .iter()
        .chain(SEMANTIC)
        .chain(COMPONENT)
        .chain(ALIASES)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const CSS: &str = include_str!("../assets/melissa.css");

    /// The custom properties declared in the stylesheet's `:root` rules, with their values.
    fn root_declarations() -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        let mut rest = CSS;
        while let Some(at) = rest.find(":root") {
            let after = &rest[at..];
            let (Some(open), Some(close)) = (after.find('{'), after.find('}')) else {
                break;
            };
            for decl in after[open + 1..close].split(';') {
                // Drop the comments in front of a declaration.
                let decl = decl.rsplit("*/").next().unwrap_or(decl).trim();
                if let Some((name, value)) = decl.split_once(':') {
                    if name.starts_with("--") {
                        out.push((name.trim(), value.trim()));
                    }
                }
            }
            rest = &after[close..];
        }
        out
    }

    #[test]
    fn every_listed_token_is_defined_and_every_defined_token_listed() {
        let defined: BTreeSet<&str> = root_declarations().iter().map(|(n, _)| *n).collect();
        let listed: BTreeSet<&str> = all().collect();
        assert_eq!(listed.len(), all().count(), "a token is listed twice");
        let missing: Vec<_> = listed.difference(&defined).collect();
        assert!(
            missing.is_empty(),
            "listed, not in melissa.css :root: {missing:?}"
        );
        let unlisted: Vec<_> = defined.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "in melissa.css :root, not listed: {unlisted:?}"
        );
    }

    #[test]
    fn layers_read_only_the_layer_below() {
        let decls = root_declarations();
        let refs = |value: &str| -> Vec<String> {
            value
                .split("var(")
                .skip(1)
                .filter_map(|v| v.split(')').next())
                .map(|v| v.trim().to_owned())
                .collect()
        };
        for (name, value) in &decls {
            let used = refs(value);
            let ok = |r: &String| {
                if PRIMITIVES.contains(name) {
                    false
                } else if SEMANTIC.contains(name) {
                    PRIMITIVES.contains(&r.as_str())
                } else {
                    SEMANTIC.contains(&r.as_str())
                }
            };
            for r in &used {
                assert!(ok(r), "{name} reads {r}");
            }
        }
        // Colours are written once, for both schemes.
        for (name, value) in &decls {
            if name.starts_with("--color-") {
                assert!(value.starts_with("light-dark("), "{name}: {value}");
            }
        }
    }
}
