Front-end assets, embedded into the binary with rust-embed (no CDN, works offline).

- `htmx.min.js`: htmx 2.0.4 from https://htmx.org (BSD Zero Clause license). Update by replacing the file and noting the version here.
- `htmx-ext-sse.min.js`: the htmx SSE extension, npm package `htmx-ext-sse` 2.2.4 (`dist/sse.min.js`), BSD Zero
  Clause license (`htmx-ext-sse.LICENSE.txt`). Downloaded 2026-11-18 from registry.npmjs.org; the tarball matched
  the registry's sha512 integrity `LJmxVhyk…dpEMqmEQ==`. Uses no eval and injects no styles.
- `fonts/`: SIL OFL 1.1 fonts with their license texts, from google/fonts at commit `7af0d90b4d53`
  (`ofl/atkinsonhyperlegiblenext`, `ofl/bricolagegrotesque`). Neither declares a Reserved Font Name. Converted with
  fontTools 4.63 (via `nix shell`):
  - `AtkinsonHyperlegibleNext-Variable.woff2`: `AtkinsonHyperlegibleNext[wght].ttf`, Latin subset (wght 200–800).
  - `BricolageGrotesque-Variable.woff2`: `BricolageGrotesque[opsz,wdth,wght].ttf` instanced at `wdth=100 opsz=32`
    (`fonttools varLib.instancer`), Latin subset (wght 200–800).
  - Subset: `pyftsubset --unicodes=U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2190-2199,U+2212,U+2215,U+FEFF,U+FFFD --layout-features='*' --flavor=woff2`.
- `melissa.css`, `melissa.js`, `mark.svg`: Melissa's own (same license as the project). `melissa.js` only adds
  Enter-to-send, scrolling and a reload after a daemon restart; everything else is htmx.
- `logos/`: provider marks, normalized to `<svg xmlns viewBox><title/><path d [fill-rule]/></svg>` (nothing
  else; `crates/web/src/logos.rs` checks every file and draws the paths itself). Downloaded 2026-11-20 from
  registry.npmjs.org: `opencode.svg`, `cursor.svg`, `cline.svg` from `simple-icons` 16.38.0 (CC0 1.0;
  integrity `sha512-B0Wws90eL0Fm65aH35CA8kM+7/Zzgsaytt0DndBx7VFm3+o9HurVIVRQZ1hvqqYYIyNJq7ycn3kQm2v2FhiWSQ==`);
  `grok.svg`, `goose.svg` from `@lobehub/icons-static-svg` 1.104.0 (MIT, `LobeHub-icons-LICENSE.txt` from
  github.com/lobehub/lobe-icons; `fill`, `style`, `width`, `height` removed, `fill-rule` moved onto the path).
  The marks are their owners' trademarks, used only to identify the provider (see ACKNOWLEDGMENTS.md).
