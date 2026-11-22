#!/usr/bin/env python3
"""Melissa's design tokens, written once, for Penpot and for the stylesheet.

    python3 scripts/tokens/tokens_gen.py json > crates/web/design/tokens.json   # `just tokens`
    python3 scripts/tokens/tokens_gen.py css                                    # a :root draft
    python3 scripts/tokens/tokens_gen.py counts

`json` writes Penpot's token file (DTCG style, several sets in one file): `core` (primitives
`ref.*`, sizes, type, motion and the component tokens) and `theme/light`, `theme/dark` (the
semantic colours). Penpot imports it as is (Tokens → Import); its themes "Mode/Light" and
"Mode/Dark" pick one theme set each.

`css` prints the same tokens as custom properties for `crates/web/assets/melissa.css`'s
`:root`, colours as `light-dark(light, dark)`. The stylesheet is hand-written; this is a
starting point when many tokens change at once. A test in `crates/web/src/tokens.rs` fails
when the stylesheet and `crates/web/design/tokens.json` disagree (docs/DEV-ENV.md).

Names: Penpot `a.b.c` is the CSS variable `--a-b-c`. Values: Penpot numbers are px;
the stylesheet writes space and font sizes in rem (px / 16) and durations in ms.
"""
import json
import sys

core, light, dark = {}, {}, {}
flat = {'core': [], 'theme/light': [], 'theme/dark': []}


def put(tree, path, typ, value, desc=None):
    node = tree
    parts = path.split('.')
    for p in parts[:-1]:
        node = node.setdefault(p, {})
    leaf = {'$value': value, '$type': typ}
    if desc:
        leaf['$description'] = desc
    node[parts[-1]] = leaf


def c(path, typ, value, desc=None):
    put(core, path, typ, value, desc)
    flat['core'].append((path, typ, value))


def l(path, value, typ='color'):
    put(light, path, typ, value)
    flat['theme/light'].append((path, typ, value))


def d(path, value, typ='color'):
    put(dark, path, typ, value)
    flat['theme/dark'].append((path, typ, value))


# ---- Layer 1: primitives (ref.*) ------------------------------------------------------------
ref = {
    # light greys
    'ref.neutral.0': '#ffffff', 'ref.neutral.50': '#f2f4f6', 'ref.neutral.100': '#eef1f5', 'ref.neutral.150': '#e6eaef',
    'ref.neutral.200': '#dce1e7', 'ref.neutral.400': '#9aa5b4', 'ref.neutral.500': '#7a8594', 'ref.neutral.600': '#566273',
    'ref.neutral.900': '#1b2533', 'ref.neutral.1000': '#000000',
    # dark greys
    'ref.night.100': '#e6eaf0', 'ref.night.400': '#a3adbb', 'ref.night.600': '#6b7686', 'ref.night.700': '#5d6878',
    'ref.night.750': '#2d3a4e', 'ref.night.800': '#2e3846', 'ref.night.850': '#222b37', 'ref.night.900': '#1a212b',
    'ref.night.950': '#12171e', 'ref.night.1000': '#0e1217',
    # accent blue
    'ref.blue.50': '#e8effc', 'ref.blue.100': '#c4d6fa', 'ref.blue.200': '#a9c3f8', 'ref.blue.300': '#8db0f5',
    'ref.blue.600': '#1f55c2', 'ref.blue.700': '#1a48a6', 'ref.blue.800': '#163d8c', 'ref.blue.950': '#1a2640',
    'ref.blue.ink': '#0b1220',
    # honey (brand, attention)
    'ref.honey.100': '#f7edd8', 'ref.honey.300': '#e9b956', 'ref.honey.400': '#d99a1e', 'ref.honey.500': '#c98a0b',
    'ref.honey.700': '#7a5200', 'ref.honey.900': '#2b2416',
    # rust (caution)
    'ref.rust.100': '#fbece4', 'ref.rust.150': '#f8c4ab', 'ref.rust.200': '#f6b291', 'ref.rust.300': '#f39a70',
    'ref.rust.700': '#a8430f', 'ref.rust.800': '#8f390d', 'ref.rust.850': '#7a300b', 'ref.rust.900': '#2d1d16',
    'ref.rust.950': '#1f0e05',
    # green (ok)
    'ref.green.100': '#e3f2e9', 'ref.green.300': '#6cc893', 'ref.green.600': '#2e7d4f', 'ref.green.900': '#17291f',
    # overlays (hex + alpha)
    'ref.black.a18': '#0000002e', 'ref.black.a45': '#00000073', 'ref.backdrop.a45': '#0a0e1473', 'ref.backdrop.a60': '#0a0e1499',
}
for k, v in ref.items():
    c(k, 'color', v)
identity = {  # helpers' icon colours: (light, dark)
    'green': ('#2f7d4a', '#6cc893'), 'teal': ('#0f7676', '#4fc6c0'), 'blue': ('#2a5bd7', '#8db0f5'),
    'violet': ('#6b4bc4', '#b49cf2'), 'rose': ('#b8395f', '#f08aab'), 'orange': ('#b4540f', '#f2a25c'),
    'lime': ('#5a7a0c', '#b5d55a'), 'slate': ('#5b6b80', '#a3adbb'),
}
for name, (lv, dv) in identity.items():
    c(f'ref.identity.{name}.light', 'color', lv)
    c(f'ref.identity.{name}.dark', 'color', dv)

# ---- Dimensions, type, motion (theme-independent: core) -------------------------------------
for i, v in zip(range(1, 8), [4, 8, 12, 16, 24, 32, 48]):
    c(f'space.{i}', 'spacing', str(v))
c('space.half', 'spacing', '2')
for k, v in {'xs': 4, 'sm': 6, 'md': 8, 'lg': 12, 'full': 999}.items():
    c(f'radius.{k}', 'borderRadius', str(v))
for k, v in {'sm': 28, 'md': 32, 'lg': 40}.items():
    c(f'size.control.{k}', 'sizing', str(v))
for k, v in {'sm': 32, 'md': 40, 'lg': 56}.items():
    c(f'size.row.{k}', 'sizing', str(v))
for k, v in {'sm': 16, 'md': 20, 'lg': 24}.items():
    c(f'size.icon.{k}', 'sizing', str(v))
c('size.target.min', 'sizing', '24', 'fine pointer; 44 under @media (pointer: coarse)')
c('size.target.touch', 'sizing', '44', 'value of size.target.min under @media (pointer: coarse)')
c('font.family.body', 'fontFamilies', 'Atkinson Hyperlegible Next')
c('font.family.head', 'fontFamilies', 'Bricolage Grotesque')
c('font.family.mono', 'fontFamilies', 'ui-monospace')
for k, v in {'xs': 12.8, 'sm': 13.6, 'md': 16, 'lg': 18.4, 'xl': 28}.items():
    c(f'font.size.{k}', 'fontSizes', str(v))
for k, v in {'sm': 13.6, 'md': 14.4, 'lg': 16}.items():
    c(f'font.size.control.{k}', 'fontSizes', str(v))
for k, v in {'regular': 400, 'strong': 600, 'heading': 650, 'brand': 700}.items():
    c(f'font.weight.{k}', 'fontWeights', str(v))
for k, v in {'tight': 1.2, 'control': 1.25, 'body': 1.5}.items():
    c(f'line.height.{k}', 'number', str(v))
c('border.width.hairline', 'borderWidth', '1')
c('focus.width', 'borderWidth', '3')
c('focus.offset', 'dimension', '2')
for k, v in {'fast': 150, 'medium': 200, 'enter': 180}.items():
    c(f'motion.duration.{k}', 'number', str(v), 'milliseconds')

# ---- Layer 2: semantic colours (theme/light, theme/dark) ------------------------------------
# Dark mode (owner, 2026-11-22): hover, selected and quiet fills are one step lighter than the
# surface (night.850), so they show on the page and on cards; disabled text is dimmer than
# muted text (light and dark).
sem = [
    # token, light ref, dark ref
    ('color.bg.ground', 'neutral.50', 'night.950'),
    ('color.bg.surface', 'neutral.0', 'night.900'),
    ('color.bg.raised', 'neutral.0', 'night.850'),
    ('color.bg.sunken', 'neutral.50', 'night.950'),
    ('color.bg.hover', 'neutral.50', 'night.850'),
    ('color.bg.active', 'neutral.150', 'night.800'),
    ('color.bg.selected', 'neutral.50', 'night.850'),
    ('color.bg.fill', 'neutral.150', 'night.850'),
    ('color.text.default', 'neutral.900', 'night.100'),
    ('color.text.muted', 'neutral.600', 'night.400'),
    ('color.text.disabled', 'neutral.500', 'night.600'),
    ('color.text.link', 'blue.600', 'blue.300'),
    ('color.border.default', 'neutral.200', 'night.800'),
    ('color.border.control', 'neutral.500', 'night.600'),
    ('color.border.strong', 'neutral.600', 'night.400'),
    ('color.action.bg', 'blue.600', 'blue.300'),
    ('color.action.bg-hover', 'blue.700', 'blue.200'),
    ('color.action.bg-active', 'blue.800', 'blue.100'),
    ('color.action.text', 'neutral.0', 'blue.ink'),
    ('color.action.soft', 'blue.50', 'blue.950'),
    ('color.focus.ring', 'blue.600', 'blue.300'),
    ('color.caution.bg', 'rust.700', 'rust.300'),
    ('color.caution.bg-hover', 'rust.800', 'rust.200'),
    ('color.caution.bg-active', 'rust.850', 'rust.150'),
    ('color.caution.text', 'rust.700', 'rust.300'),
    ('color.caution.soft', 'rust.100', 'rust.900'),
    ('color.caution.on', 'neutral.0', 'rust.950'),
    ('color.ok.fg', 'green.600', 'green.300'),
    ('color.ok.soft', 'green.100', 'green.900'),
    ('color.ok.text', 'green.600', 'green.300'),
    ('color.attention.fg', 'honey.500', 'honey.400'),
    ('color.attention.soft', 'honey.100', 'honey.900'),
    ('color.attention.text', 'honey.700', 'honey.300'),
    ('color.idle.fg', 'neutral.400', 'night.700'),
    ('color.bubble.bg', 'neutral.900', 'night.750'),
    ('color.bubble.text', 'neutral.0', 'neutral.100'),
    ('color.backdrop', 'backdrop.a45', 'backdrop.a60'),
    ('color.shadow', 'black.a18', 'black.a45'),
    # providers' one-colour marks (brand rule): black, white in dark mode
    ('color.logo', 'neutral.1000', 'neutral.0'),
]
for tok, lr, dr in sem:
    l(tok, '{ref.' + lr + '}')
    d(tok, '{ref.' + dr + '}')
for name in identity:
    l(f'color.avatar.{name}', '{ref.identity.' + name + '.light}')
    d(f'color.avatar.{name}', '{ref.identity.' + name + '.dark}')

# ---- Layer 3: component tokens (core; aliases of semantic and dimension tokens) -------------
for s in ['sm', 'md', 'lg']:
    c(f'button.height.{s}', 'sizing', '{size.control.' + s + '}')
    c(f'button.font-size.{s}', 'fontSizes', '{font.size.control.' + s + '}')
c('button.padding-x.sm', 'spacing', '10')
c('button.padding-x.md', 'spacing', '{space.3}')
c('button.padding-x.lg', 'spacing', '{space.4}')
c('button.radius.sm', 'borderRadius', '{radius.sm}')
c('button.radius.md', 'borderRadius', '{radius.md}')
c('button.radius.lg', 'borderRadius', '{radius.md}')
c('button.icon.sm', 'sizing', '{size.icon.sm}')
c('button.icon.md', 'sizing', '{size.icon.sm}')
c('button.icon.lg', 'sizing', '{size.icon.md}')
btn = {
    'primary': {'bg.default': 'action.bg', 'bg.hover': 'action.bg-hover', 'bg.active': 'action.bg-active', 'bg.disabled': 'bg.fill',
                'text.default': 'action.text', 'text.disabled': 'text.disabled'},
    'secondary': {'bg.default': 'bg.surface', 'bg.hover': 'bg.hover', 'bg.active': 'bg.active', 'bg.pressed': 'bg.selected',
                  'text.default': 'text.default', 'text.disabled': 'text.disabled', 'border.default': 'border.control', 'border.hover': 'border.strong'},
    'ghost': {'bg.hover': 'bg.hover', 'bg.active': 'bg.active', 'bg.pressed': 'bg.selected', 'text.default': 'text.default', 'text.disabled': 'text.disabled'},
    'danger': {'bg.default': 'caution.bg', 'bg.hover': 'caution.bg-hover', 'bg.active': 'caution.bg-active', 'bg.disabled': 'bg.fill',
               'text.default': 'caution.on', 'text.disabled': 'text.disabled'},
    'danger-quiet': {'bg.hover': 'caution.soft', 'text.default': 'caution.text', 'text.disabled': 'text.disabled'},
    'link': {'text.default': 'text.link'},
}
for v, m in btn.items():
    for k, s in m.items():
        c(f'button.{v}.{k}', 'color', '{color.' + s + '}')
c('field.height', 'sizing', '{size.control.md}')
c('field.padding-x', 'spacing', '{space.3}')
c('field.radius', 'borderRadius', '{radius.md}')
for k, s in {'bg.default': 'bg.surface', 'bg.disabled': 'bg.sunken', 'text.default': 'text.default', 'text.disabled': 'text.disabled',
             'border.default': 'border.control', 'border.hover': 'border.strong', 'border.invalid': 'caution.text',
             'placeholder.default': 'text.muted', 'label.text': 'text.default', 'hint.text': 'text.muted', 'error.text': 'caution.text'}.items():
    c(f'field.{k}', 'color', '{color.' + s + '}')
c('field.gap', 'spacing', '{space.1}')
for k, s in {'track.bg.off': 'bg.sunken', 'track.bg.on': 'action.bg', 'track.bg.disabled': 'bg.sunken', 'track.border.off': 'border.control',
             'track.border.on': 'action.bg', 'knob.off': 'text.muted', 'knob.on': 'action.text', 'knob.disabled': 'text.disabled'}.items():
    c(f'switch.{k}', 'color', '{color.' + s + '}')
c('switch.md.width', 'sizing', '32')
c('switch.md.height', 'sizing', '18')
c('switch.sm.width', 'sizing', '28')
c('switch.sm.height', 'sizing', '16')
tones = {'neutral': ('bg.fill', 'text.muted'), 'info': ('action.soft', 'text.link'), 'ok': ('ok.soft', 'ok.text'),
         'attention': ('attention.soft', 'attention.text'), 'caution': ('caution.soft', 'caution.text')}
for t, (bg, tx) in tones.items():
    c(f'chip.{t}.bg', 'color', '{color.' + bg + '}')
    c(f'chip.{t}.text', 'color', '{color.' + tx + '}')
c('chip.height', 'sizing', '20', 'badge / status chip height; interactive chips use button.height.sm')
c('chip.font-size', 'fontSizes', '{font.size.xs}')
c('row.height.sm', 'sizing', '{size.row.sm}')
c('row.height.md', 'sizing', '{size.row.md}')
c('row.height.lg', 'sizing', '{size.row.lg}')
c('row.padding-x', 'spacing', '{space.3}')
for k, s in {'bg.hover': 'bg.hover', 'bg.active': 'bg.active', 'bg.current': 'bg.selected'}.items():
    c(f'row.{k}', 'color', '{color.' + s + '}')
for k, s in {'track.bg': 'bg.sunken', 'track.border': 'border.default', 'selected.bg': 'bg.surface', 'selected.border': 'border.control',
             'text.default': 'text.muted', 'text.selected': 'text.default'}.items():
    c(f'segmented.{k}', 'color', '{color.' + s + '}')

doc = {
    'core': core, 'theme/light': light, 'theme/dark': dark,
    '$themes': [
        {'name': 'Light', 'group': 'Mode', 'description': '', 'isSource': False,
         'selectedTokenSets': {'core': 'enabled', 'theme/light': 'enabled'}},
        {'name': 'Dark', 'group': 'Mode', 'description': '', 'isSource': False,
         'selectedTokenSets': {'core': 'enabled', 'theme/dark': 'enabled'}}],
    '$metadata': {'tokenSetOrder': ['core', 'theme/light', 'theme/dark'], 'activeThemes': ['Mode/Dark'],
                  'activeSets': ['core', 'theme/dark']},
}

# ---- CSS draft ------------------------------------------------------------------------------
FAMILIES = {
    'Atkinson Hyperlegible Next': '"Atkinson Hyperlegible Next", system-ui, sans-serif',
    'Bricolage Grotesque': '"Bricolage Grotesque", "Atkinson Hyperlegible Next", system-ui, sans-serif',
    'ui-monospace': 'ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace',
}


def css_name(path):
    return '--' + path.replace('.', '-')


def css_value(path, typ, value):
    if value.startswith('{') and value.endswith('}'):
        return f'var({css_name(value[1:-1])})'
    if typ in ('spacing', 'fontSizes'):
        return f'{float(value) / 16:g}rem'
    if typ in ('sizing', 'borderRadius', 'borderWidth', 'dimension'):
        return f'{value}px'
    if typ == 'number' and path.startswith('motion.duration'):
        return f'{value}ms'
    if typ == 'fontFamilies':
        return FAMILIES[value]
    return value


def css():
    out = [':root {']
    for path, typ, value in flat['core']:
        out.append(f'  {css_name(path)}: {css_value(path, typ, value)};')
    darks = {p: v for p, _, v in flat['theme/dark']}
    for path, typ, value in flat['theme/light']:
        lv, dv = css_value(path, typ, value), css_value(path, typ, darks[path])
        out.append(f'  {css_name(path)}: light-dark({lv}, {dv});')
    out.append('}')
    return '\n'.join(out)


if __name__ == '__main__':
    mode = sys.argv[1] if len(sys.argv) > 1 else 'json'
    if mode == 'json':
        print(json.dumps(doc, indent=2))
    elif mode == 'css':
        print(css())
    elif mode == 'flat':
        print(json.dumps(flat, separators=(',', ':')))
    elif mode == 'counts':
        prim = sum(1 for p, _, _ in flat['core'] if p.startswith('ref.'))
        comp = sum(1 for p, _, _ in flat['core'] if p.split('.')[0] in ('button', 'field', 'switch', 'chip', 'row', 'segmented'))
        dims = len(flat['core']) - prim - comp
        print({'primitives(ref)': prim, 'semantic dims/type (core)': dims, 'semantic colour per theme': len(flat['theme/light']),
               'component': comp, 'total': len(flat['core']) + len(flat['theme/light']) + len(flat['theme/dark'])})
    else:
        sys.exit(f'unknown mode {mode!r}: json, css, flat or counts')
