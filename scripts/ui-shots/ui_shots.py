#!/usr/bin/env python3
"""UI screenshot harness: before/after pictures of every Melissa screen.

  ui_shots.py shoot [--rev REV] [--out DIR] [--bin PATH] [--only TEXT]
      Start a throwaway Melissa from source (built binary), seed fixed content, take PNGs of
      every screen at 1440, 1024 and 390 px wide in dark and light, write them and an
      index.html contact sheet to target/ui-shots/<revision>/. With --rev, the revision is
      checked out in a temporary git worktree, built there and shot with this harness.
  ui_shots.py diff A B [--threshold PCT] [--tolerance N] [--fail]
      Compare two shot sets (directories, or revisions shot before / shot now): per-screen
      diff images, a summary (changed %, list) and index.html in target/ui-shots/diff-A-B/.

Offline: the only network use is the browser talking to the throwaway server on localhost.
See docs/DEV-ENV.md ("UI screenshots").
"""

import argparse
import html
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True  # no __pycache__ in the source tree

import server as srv  # noqa: E402

REPO = HERE.parent.parent
SHOTS = REPO / "target" / "ui-shots"

WIDTHS = {1440: 900, 1024: 768, 390: 844}
# The server's fake app tools (crates/web/src/fake_apps.rs): the Apps list without k3s.
FAKE_APPS = "melissa-server/fake-apps"
FAKE_USAGE = "melissa-server/fake-usage"
FAKES = f"{FAKE_APPS},{FAKE_USAGE}"
SCHEMES = ("dark", "light")

# Software raster on one thread, every compositor stage before a frame is drawn: without
# these, anti-aliased edges at fractional positions now and then differ between runs.
CHROMIUM_ARGS = [
    "--hide-scrollbars",
    "--font-render-hinting=none",
    "--disable-lcd-text",
    "--force-color-profile=srgb",
    "--disable-gpu",
    "--num-raster-threads=1",
    "--disable-partial-raster",
    "--run-all-compositor-stages-before-draw",
    "--disable-checker-imaging",
    "--disable-threaded-animation",
    "--disable-threaded-scrolling",
]

# Freeze what moves: no transitions, animations or blinking caret.
STILL_CSS = """
*, *::before, *::after {
  animation: none !important;
  transition: none !important;
  caret-color: transparent !important;
  scroll-behavior: auto !important;
}
"""


# --- host facts the UI shows (hostname, OS, kernel, uptime, memory) and times relative to the
# --- start of the run: replaced in the page before a shot ---

def host_facts():
    facts = []
    host = socket.gethostname().split(".")[0]
    if host:
        facts.append(host)
    try:
        for line in Path("/etc/os-release").read_text().splitlines():
            key, _, value = line.partition("=")
            if key in ("PRETTY_NAME", "NAME", "VERSION", "VERSION_ID"):
                value = value.strip().strip('"')
                if value:
                    facts.append(value)
    except OSError:
        pass
    facts.append(platform.release())
    # Longest first, so "Fedora Linux 45 (Kinoite)" goes before "Fedora Linux".
    return sorted({f for f in facts if len(f) >= 3}, key=len, reverse=True)


NORMALIZE_JS = """
(facts) => {
  const esc = (s) => s.replace(/[.*+?^${}()|[\\]\\\\]/g, '\\\\$&');
  const rules = facts.map((f) => [new RegExp('\\b' + esc(f) + '\\b', 'gi'), 'example-host']);
  rules.push([/Up( \\d+ (d|h|min|s)\\b)+/g, 'Up 1 h 0 min']);
  // How long ago the run started differs by seconds between runs ("Checked 2 min ago").
  rules.push([/\\b\\d+ (s|sec|seconds?|min|minutes?) ago\\b/g, 'just now']);
  // Usage limits that reset within a day count down from the check.
  rules.push([/Resets in( \\d+(\\s|\\u00a0)(h|min))+/g, 'Resets in 2 h 10 min']);
  rules.push([/\\d+(\\.\\d+)? [KMGT]iB of \\d+(\\.\\d+)? [KMGT]iB used/g, '4.0 GiB of 16 GiB used']);
  const fix = (s) => rules.reduce((t, [re, to]) => t.replace(re, to), s);
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const t = fix(n.nodeValue);
    if (t !== n.nodeValue) n.nodeValue = t;
  }
  for (const el of document.querySelectorAll('input, textarea')) {
    if (el.value) el.value = fix(el.value);
    if (el.placeholder) el.placeholder = fix(el.placeholder);
  }
  for (const m of document.querySelectorAll('.device meter, .meter meter')) m.value = 25;
}
"""


# Settings and other panels scroll inside a box, not the page: the content below the fold is
# shot as extra pages (`<name>~2`, ...) by scrolling the largest scrolled box that starts at
# its top (a chat starts at its bottom and gets no extra pages).
EXTRA_PAGES = 3
SCROLL_NEXT_JS = """
() => {
  let el = document.querySelector('[data-shots-scroll]');
  if (!el) {
    let best = null, area = 0;
    for (const c of document.querySelectorAll('body *')) {
      const oy = getComputedStyle(c).overflowY;
      if ((oy === 'auto' || oy === 'scroll') && c.scrollHeight > c.clientHeight + 4
          && c.clientHeight > 100 && c.clientWidth * c.clientHeight > area) {
        best = c; area = c.clientWidth * c.clientHeight;
      }
    }
    if (!best || best.scrollTop > 0) return false;
    el = best;
    el.setAttribute('data-shots-scroll', '');
  }
  const before = el.scrollTop;
  el.scrollTop = before + el.clientHeight - 40;
  return el.scrollTop > before;
}
"""


# --- the screens -------------------------------------------------------------------------

class Skip(Exception):
    """This screen does not exist at this width or on this install (not a failure)."""


def present(page, locator, what):
    try:
        locator.first.wait_for(state="visible", timeout=2000)
    except Exception:
        raise Skip(f"no visible {what}")
    return locator.first


def click(selector):
    def act(page):
        present(page, page.locator(selector), selector).click()
        page.wait_for_timeout(150)
    return act


def open_details(page):
    toggle = present(page, page.locator("[data-details-toggle]"), "details toggle")
    if toggle.get_attribute("aria-expanded") != "true":
        toggle.click()
        page.wait_for_timeout(150)


def open_confirm(kind, label):
    """Open an in-page confirmation (`confirm-inline` or `danger-confirm`) by its summary."""
    def act(page):
        summary = page.locator(f"details.{kind} > summary", has_text=re.compile(label))
        present(page, summary, f"{kind} {label}").click()
        page.wait_for_timeout(150)
    return act


def show_person(name):
    """The owner's Apps filter: show `name`'s apps (swaps the list in place)."""
    def act(page):
        link = page.locator(".show-filter a", has_text=re.compile(f"^{name}$"))
        present(page, link, f"filter {name}").click()
        page.wait_for_timeout(400)
    return act


def usage_loaded(page):
    """Usage: wait until the providers' part has loaded (it loads after the page)."""
    page.locator("#usage-providers:not([aria-busy])").wait_for()
    page.wait_for_timeout(100)


# Screens seen by a member (scripts/ui-shots/server.py MEMBERS): (member, name, path, action).
MEMBER_SCREENS = [
    ("ben", "apps-member", "/cluster", None),
    ("ben", "app-details-member", "/cluster/apps/minecraft", None),
    ("cy", "apps-empty", "/cluster", None),
    ("ben", "usage-member", "/usage", None),
]


def fill(selector, value):
    def act(page):
        page.locator(selector).first.fill(value)
    return act


def scroll_chat_top(page):
    """Scroll every scrolled box (the chat feed) to its top."""
    page.evaluate("""() => {
      for (const el of document.querySelectorAll('main *, body *')) {
        if (el.scrollHeight > el.clientHeight + 4 && getComputedStyle(el).overflowY !== 'visible')
          el.scrollTop = 0;
      }
    }""")
    page.wait_for_timeout(200)


def screens(helper, quiet):
    """(name, path, action or None). `helper` and `quiet` are helper ids."""
    s = [
        ("chat-melissa", "/", None),
        ("chat-melissa-top", "/", scroll_chat_top),
        ("chat-helper", f"/agents/{helper}", None),
        ("chat-empty", f"/agents/{quiet}", None),
        ("sidebar-collapsed", "/", click("[data-sidebar-collapse]")),
        ("details-panel", f"/agents/{helper}", open_details),
        ("menu-agent", f"/agents/{helper}", click("button[popovertarget=menu-head]")),
        ("menu-device", "/", click("button[popovertarget=device-menu]")),
        ("model-picker", f"/agents/{helper}", click("button.model-chip")),
        ("approvals", "/approvals", None),
        ("apps", "/cluster", None),
        ("apps-by-person", "/cluster", show_person("ben")),
        ("app-details", "/cluster/apps/minecraft", None),
        ("usage", "/usage", usage_loaded),
        ("usage-cost", "/usage?view=cost", usage_loaded),
        ("usage-tokens", "/usage?view=tokens", usage_loaded),
        ("add-provider", "/settings/providers/add", None),
        ("settings-provider", "/settings/providers?p=python3", None),
        ("settings-helper", f"/settings/agents/{helper}", None),
        ("settings-new-helper", "/settings/agents/new", None),
        ("dialog-delete-helper", f"/settings/agents/{quiet}",
         open_confirm("confirm-inline", "Delete")),
        ("dialog-reset", "/settings/reset", open_confirm("danger-confirm", "Reset")),
    ]
    return s


SETTINGS_SECTIONS = ("account", "people", "agents", "providers", "permissions", "schedules",
                     "apps", "files", "voice", "tailscale", "updates", "developer", "reset")


# --- running ------------------------------------------------------------------------------

def git(*args, cwd=REPO):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout.strip()


def revision_name(cwd=REPO):
    rev = git("rev-parse", "--short=10", "HEAD", cwd=cwd)
    dirty = subprocess.run(["git", "diff", "--quiet", "HEAD", "--", "crates"], cwd=cwd).returncode
    return f"{rev}-dirty" if dirty else rev


def log(msg):
    print(f"ui-shots: {msg}", flush=True)


class Shooter:
    def __init__(self, out: Path, binary: Path, only: str | None):
        self.out = out
        self.binary = binary
        self.only = only
        self.facts = host_facts()
        self.taken = []  # (name, width, scheme, file)
        self.failed = []  # (name, variant, error)
        self.skipped = []  # (name, variant, why): not on this width or install

    def wanted(self, name):
        return self.only is None or self.only in name

    def context(self, browser, width, scheme, state=None):
        ctx = browser.new_context(
            viewport={"width": width, "height": WIDTHS[width]},
            device_scale_factor=1,
            color_scheme=scheme,
            reduced_motion="reduce",
            bypass_csp=True,  # for the style that stops animations
            locale="en-GB",
            timezone_id="UTC",
            storage_state=state,
            service_workers="block",
        )
        ctx.set_default_timeout(5000)
        return ctx

    def settle(self, page):
        page.wait_for_load_state("load")
        page.add_style_tag(content=STILL_CSS)
        page.evaluate("document.fonts.ready.then(() => true)")
        page.wait_for_timeout(250)
        page.evaluate(NORMALIZE_JS, self.facts)

    def shoot(self, page, name, width, scheme):
        file = self.out / f"{name}--{width}-{scheme}.png"
        page.screenshot(path=str(file), full_page=True, animations="disabled", caret="hide",
                        scale="css")
        self.taken.append((name, width, scheme, file.name))

    def visit_all(self, browser, base, state, plan):
        """Each (name, path, action) at every width and scheme."""
        plan = [p for p in plan if self.wanted(p[0])]
        if not plan:
            return
        for width in WIDTHS:
            for scheme in SCHEMES:
                for name, path, action in plan:
                    # A fresh context per shot: nothing a screen remembers in the browser
                    # (collapsed sidebar, drafts, favorites) leaks into the next one.
                    ctx = self.context(browser, width, scheme, state)
                    page = ctx.new_page()
                    try:
                        page.goto(base + path)
                        self.settle(page)
                        if action:
                            action(page)
                            page.evaluate(NORMALIZE_JS, self.facts)
                            page.wait_for_timeout(100)
                        self.shoot(page, name, width, scheme)
                        for n in range(2, 2 + EXTRA_PAGES):
                            if not page.evaluate(SCROLL_NEXT_JS):
                                break
                            page.wait_for_timeout(100)
                            page.evaluate(NORMALIZE_JS, self.facts)
                            self.shoot(page, f"{name}~{n}", width, scheme)
                    except Skip as e:
                        self.skipped.append((name, f"{width}-{scheme}", str(e)))
                    except Exception as e:  # one screen failing must not stop the run
                        lines = [l.strip() for l in str(e).splitlines() if l.strip()]
                        why = [l for l in lines if "intercepts" in l or "not visible" in l
                               or "not stable" in l or "waiting for" in l]
                        msg = "; ".join(lines[:1] + why[-2:])
                        self.failed.append((name, f"{width}-{scheme}", msg))
                        log(f"{name} {width}-{scheme}: {msg}")
                    finally:
                        ctx.close()

    def run(self):
        from playwright.sync_api import sync_playwright

        self.out.mkdir(parents=True, exist_ok=True)
        # A full run replaces the set; --only replaces just the shots it takes again.
        for old in self.out.glob("*.png"):
            if self.only is None or self.only in old.name.split("--")[0]:
                old.unlink()
        server = srv.Server(self.binary, self.out / "server.log")
        started = time.time()
        env = dict(os.environ, FONTCONFIG_FILE=os.environ["MELISSA_SHOTS_FONTCONFIG"])
        try:
            server.start()
            self.server = server
            log(f"server on {server.url} (state in {server.root})")
            base = server.url
            with sync_playwright() as p:
                browser = p.chromium.launch(env=env, args=CHROMIUM_ARGS)
                state_file = server.root / "storage.json"
                self.visit_all(browser, base, None, [("claim", "/claim", None)])
                self.claim(browser, base, state_file)
                self.visit_all(browser, base, None, [("sign-in", "/login", None)])
                self.visit_all(browser, base, str(state_file), [
                    ("setup-device", "/setup/device", fill("input[name=name]", "Melissa box")),
                    ("setup-provider", "/setup/provider", None),
                    ("setup-model", "/setup/model", None),
                    ("setup-extras", "/setup/extras", None),
                ])
                self.finish_setup(browser, base, state_file)
                self.seed_chats(browser, base, state_file)
                helper, quiet = srv.helper_id("Backup"), srv.helper_id("Updates")
                plan = screens(helper, quiet)
                # `/settings` alone is the section list on a phone (level 1).
                plan += [("settings", "/settings", None)]
                plan += [(f"settings-{s}", f"/settings/{s}", None) for s in SETTINGS_SECTIONS]
                members = self.invite_members(browser, base, state_file)
                self.visit_all(browser, base, str(state_file), plan)
                for who, name, path, action in MEMBER_SCREENS:
                    self.visit_all(browser, base, str(members[who]), [(name, path, action)])
                # Without an app platform (k3s not set up): the list says so.
                server.apps_platform(False)
                self.visit_all(browser, base, str(state_file),
                               [("apps-not-set-up", "/cluster", None)])
                server.apps_platform(True)
                browser.close()
        finally:
            server.stop()
        self.write_index()
        took = time.time() - started
        log(f"{len(self.taken)} shots in {took:.0f}s -> {self.out}")
        if self.skipped:
            log(f"{len(self.skipped)} shots skipped: not on that width or this install")
        if self.failed:
            log(f"{len(self.failed)} shots failed (listed in index.html)")
        return not self.failed

    def claim(self, browser, base, state_file):
        """Claim the box as the owner; save the signed-in browser state for the shots."""
        ctx = self.context(browser, 1440, "dark")
        page = ctx.new_page()
        page.goto(base + "/claim")
        page.fill("input[name=code]", self.server.setup_code)
        page.fill("input[name=username]", srv.OWNER)
        page.fill("input[name=password]", srv.PASSWORD)
        page.locator("form button[type=submit]").first.click()
        page.wait_for_url(re.compile(r".*/setup/.*"))
        ctx.storage_state(path=str(state_file))
        ctx.close()

    def finish_setup(self, browser, base, state_file):
        """Go through the setup steps with "Later" and "Finish setup"."""
        ctx = self.context(browser, 1440, "dark", str(state_file))
        page = ctx.new_page()
        page.goto(base + "/")
        for _ in range(8):
            page.wait_for_load_state("load")
            if "/setup/" not in page.url:
                break
            later = page.get_by_role("button", name=re.compile("^(Later|Finish setup)$"))
            link = page.get_by_role("link", name=re.compile("^(Later|Finish setup)$"))
            target = later if later.count() else link
            with page.expect_navigation():
                target.first.click()
        else:
            raise RuntimeError(f"setup did not finish (at {page.url})")
        ctx.storage_state(path=str(state_file))
        ctx.close()

    def invite_members(self, browser, base, state_file):
        """Invite each of srv.MEMBERS as the owner and join as them; their signed-in browser
        states, by name."""
        states = {}
        for name, password in srv.MEMBERS:
            ctx = self.context(browser, 1440, "dark", str(state_file))
            page = ctx.new_page()
            page.goto(base + "/settings/people")
            page.get_by_role("button", name="Create invite link").click()
            page.get_by_text(re.compile(r"https?://\S+/invite/\S")).first.wait_for()
            url = re.search(r"https?://[^\s<\"]+/invite/[^\s<\"]+", page.content()).group(0)
            ctx.close()
            ctx = self.context(browser, 1440, "dark")
            page = ctx.new_page()
            page.goto(url)
            page.fill("input[name=username]", name)
            page.fill("input[name=password]", password)
            page.fill("input[name=again]", password)
            with page.expect_navigation():
                page.locator("form button[type=submit]").first.click()
            path = self.server.root / f"storage-{name}.json"
            ctx.storage_state(path=str(path))
            ctx.close()
            states[name] = path
        return states

    def send(self, page, text):
        page.locator("#composer textarea[name=text]").fill(text)
        page.locator("#composer button.send").click()

    def seed_chats(self, browser, base, state_file):
        """Chats through the scripted agent (scripts/ui-shots/fake_agent.py)."""
        ctx = self.context(browser, 1440, "dark", str(state_file))
        page = ctx.new_page()
        # A helper: one exchange, then a model switch (a notice in the chat).
        page.goto(base + f"/agents/{srv.helper_id('Backup')}")
        self.send(page, "Hello. What do you look after?")
        page.get_by_text("I keep an eye on the apps").first.wait_for()
        page.locator("button.model-chip").click()
        page.locator("#model-picker").get_by_text("Shots Small").first.click()
        page.wait_for_timeout(500)
        # Melissa: steps and a Markdown reply, then a permission card that keeps the turn
        # running, then a queued message.
        page.goto(base + "/")
        self.send(page, "How much disk space is left?")
        page.get_by_text("podman image prune").first.wait_for()
        self.send(page, "Please clean up the old images.")
        page.get_by_text("That needs your OK").first.wait_for()
        page.wait_for_timeout(500)
        self.send(page, "Then check the logs.")
        page.locator("#chat-queue").get_by_text("Then check the logs.").first.wait_for()
        ctx.close()

    def write_index(self):
        # Every shot in the set (an --only run keeps the others), in the order taken.
        found = []
        for f in sorted(self.out.glob("*--*-*.png")):
            m = re.fullmatch(r"(.+)--(\d+)-(dark|light)\.png", f.name)
            if m:
                found.append((m[1], int(m[2]), m[3], f.name))
        order = {}
        for name, *_rest in self.taken:
            order.setdefault(name, len(order))
        found.sort(key=lambda t: (order.get(t[0], len(order)), t[0]))
        names = []
        for name, *_ in found:
            if name not in names:
                names.append(name)
        variants = [(w, s) for w in WIDTHS for s in SCHEMES]
        have = {(n, w, s): f for n, w, s, f in found}
        rows = []
        for name in names:
            cells = []
            for w, s in variants:
                f = have.get((name, w, s))
                cells.append(f'<td><a href="{f}"><img src="{f}" loading="lazy" alt=""></a></td>'
                             if f else "<td>-</td>")
            rows.append(f"<tr><th>{html.escape(name)}</th>{''.join(cells)}</tr>")
        head = "".join(f"<th>{w} {s}</th>" for w, s in variants)
        failed = "".join(f"<li>{html.escape(n)} {v}: {html.escape(e)}</li>"
                         for n, v, e in self.failed)
        skipped = "".join(f"<li>{html.escape(n)} {v}: {html.escape(e)}</li>"
                          for n, v, e in self.skipped)
        page = PAGE.format(
            title=html.escape(f"UI shots {self.out.name}"),
            body=(f"<h1>UI shots: {html.escape(self.out.name)}</h1>"
                  f"<p>{len(found)} shots, {len(names)} screens.</p>"
                  + (f"<h2>Failed</h2><ul>{failed}</ul>" if failed else "")
                  + (f"<h2>Not on this width or install</h2><ul>{skipped}</ul>"
                     if skipped else "")
                  + f"<table><tr><th></th>{head}</tr>{''.join(rows)}</table>"),
        )
        (self.out / "index.html").write_text(page)


PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>{title}</title>
<style>
body {{ font: 14px system-ui, sans-serif; margin: 16px; background: #f4f4f5; color: #18181b; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid #d4d4d8; padding: 4px; vertical-align: top; text-align: left; }}
th {{ position: sticky; top: 0; background: #e4e4e7; }}
tr th:first-child {{ position: sticky; left: 0; }}
img {{ width: 220px; display: block; }}
.changed {{ color: #b91c1c; font-weight: 600; }}
</style></head><body>{body}</body></html>
"""


# --- diff ---------------------------------------------------------------------------------

def resolve_set(spec):
    """A shot directory, or a revision (shot now if it has no shots yet)."""
    p = Path(spec)
    if p.is_dir():
        return p
    if (SHOTS / spec).is_dir():
        return SHOTS / spec
    try:
        rev = git("rev-parse", "--short=10", spec)
    except subprocess.CalledProcessError:
        raise SystemExit(f"ui-shots: {spec} is neither a shot directory nor a git revision")
    if (SHOTS / rev).is_dir():
        return SHOTS / rev
    log(f"no shots for {spec} yet; shooting {rev}")
    code = shoot_revision(argparse.Namespace(rev=rev, out=None, only=None))
    if code:
        log(f"some shots of {rev} failed")
    return SHOTS / rev


def main_diff(args):
    from PIL import Image, ImageChops

    a, b = resolve_set(args.a), resolve_set(args.b)
    out = Path(args.out) if args.out else SHOTS / f"diff-{a.name}-vs-{b.name}"
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    names = sorted({f.name for f in a.glob("*.png")} | {f.name for f in b.glob("*.png")})
    results = []  # (file, status, percent)
    for name in names:
        fa, fb = a / name, b / name
        if not fa.exists() or not fb.exists():
            results.append((name, "only in " + (a.name if fa.exists() else b.name), 100.0))
            continue
        ia, ib = Image.open(fa).convert("RGB"), Image.open(fb).convert("RGB")
        w, h = max(ia.width, ib.width), max(ia.height, ib.height)
        if ia.size != ib.size:
            pa, pb = Image.new("RGB", (w, h), (255, 0, 255)), Image.new("RGB", (w, h), (255, 0, 255))
            pa.paste(ia), pb.paste(ib)
            ia, ib = pa, pb
        delta = ImageChops.difference(ia, ib).convert("L")
        mask = delta.point(lambda v: 255 if v > args.tolerance else 0)
        changed = mask.histogram()[255]
        percent = 100.0 * changed / (w * h)
        size = "" if Image.open(fa).size == Image.open(fb).size else " (size changed)"
        if changed == 0:
            results.append((name, "same", 0.0))
            continue
        # before | after | after, dimmed, with the changed pixels in red
        faded = Image.blend(ib, Image.new("RGB", (w, h), (255, 255, 255)), 0.7)
        faded.paste(Image.new("RGB", (w, h), (230, 0, 0)), mask=mask)
        sheet = Image.new("RGB", (w * 3 + 16, h), (128, 128, 128))
        sheet.paste(ia, (0, 0)), sheet.paste(ib, (w + 8, 0)), sheet.paste(faded, (2 * w + 16, 0))
        sheet.save(out / name, optimize=False, compress_level=1)
        results.append((name, "changed" + size, percent))
    flagged = [r for r in results if r[1] != "same" and r[2] > args.threshold]
    lines = [f"ui-shots diff: {a.name} -> {b.name}",
             f"{len(names)} shots, {len(flagged)} changed above {args.threshold}% "
             f"(pixel tolerance {args.tolerance})"]
    for name, status, percent in sorted(flagged, key=lambda r: -r[2]):
        lines.append(f"  {percent:7.3f}%  {name}  {status}")
    summary = "\n".join(lines)
    (out / "summary.txt").write_text(summary + "\n")
    rows = "".join(
        f'<tr><td class="changed">{p:.3f}%</td><td>{html.escape(n)}</td><td>{html.escape(s)}</td>'
        f'<td>{"<a href=%s><img src=%s alt=></a>" % (n, n) if (out / n).exists() else ""}</td></tr>'
        for n, s, p in sorted(flagged, key=lambda r: -r[2]))
    (out / "index.html").write_text(PAGE.format(
        title=html.escape(f"UI diff {a.name} vs {b.name}"),
        body=(f"<h1>{html.escape(a.name)} &rarr; {html.escape(b.name)}</h1>"
              f"<p>{len(names)} shots, {len(flagged)} changed above {args.threshold}%. "
              "Each image: before | after | changed pixels in red.</p>"
              f"<table><tr><th>changed</th><th>shot</th><th>status</th><th>diff</th></tr>"
              f"{rows}</table>")))
    print(summary)
    print(f"ui-shots: details in {out / 'index.html'}")
    return 1 if (flagged and args.fail) else 0


def main_shoot(args):
    if args.rev:
        return shoot_revision(args)
    binary = Path(args.bin) if args.bin else REPO / "target" / "debug" / "melissa-server"
    if not args.bin:
        log("building melissa-server")
        subprocess.run(["cargo", "build", "-q", "--bins", "-p", "melissa-server",
                        "--features", FAKES], cwd=REPO, check=True)
    name = args.name or revision_name()
    out = Path(args.out) if args.out else SHOTS / name
    ok = Shooter(out, binary, args.only).run()
    return 0 if ok else 1


def shoot_revision(args):
    """Check out REV in a temporary worktree, build it and shoot it with this harness."""
    rev = git("rev-parse", "--short=10", args.rev)
    out = Path(args.out) if args.out else SHOTS / rev
    tree = SHOTS / ".worktrees" / rev
    if tree.exists():
        subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=REPO)
    tree.parent.mkdir(parents=True, exist_ok=True)
    git("worktree", "add", "--detach", str(tree), rev)
    try:
        log(f"building {rev} in {tree}")
        # This checkout's target dir: the dependencies are already built there, so only
        # Melissa's own crates are compiled (a separate target dir would cost gigabytes).
        target = REPO / "target"
        env = dict(os.environ, CARGO_TARGET_DIR=str(target))
        build = ["cargo", "build", "-q", "--bins", "-p", "melissa-server"]
        # Revisions from before the fake apps or the fake usage (2026-11-22) build without them.
        for features in ([FAKES], [FAKE_APPS], []):
            extra = ["--features", *features] if features else []
            if subprocess.run(build + extra, cwd=tree, env=env).returncode == 0:
                break
        else:
            raise RuntimeError(f"building {rev} failed")
        binary = SHOTS / ".worktrees" / f"melissa-server-{rev}"
        shutil.copy2(target / "debug" / "melissa-server", binary)
        ok = Shooter(out, binary, args.only).run()
    finally:
        subprocess.run(["git", "worktree", "remove", "--force", str(tree)], cwd=REPO)
        (SHOTS / ".worktrees" / f"melissa-server-{rev}").unlink(missing_ok=True)
    return 0 if ok else 1


def main():
    missing = [v for v in ("PLAYWRIGHT_BROWSERS_PATH", "MELISSA_SHOTS_FONTCONFIG",
                           "MELISSA_SHOTS_FAKETIME_LIB") if not os.environ.get(v)]
    if missing:
        raise SystemExit("ui-shots: run it in the dev shell (`devenv shell -- just ui-shots`); "
                         f"not set: {', '.join(missing)}")
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    shoot = sub.add_parser("shoot", help="take the screenshots")
    shoot.add_argument("--rev", help="shoot this git revision (temporary worktree)")
    shoot.add_argument("--out", help="output directory (default target/ui-shots/<revision>)")
    shoot.add_argument("--name", help="set name instead of the revision")
    shoot.add_argument("--bin", help="use this melissa-server binary (no build)")
    shoot.add_argument("--only", help="only screens whose name contains this")
    diff = sub.add_parser("diff", help="compare two shot sets")
    diff.add_argument("a", help="before: a shot directory or a git revision")
    diff.add_argument("b", help="after: a shot directory or a git revision")
    diff.add_argument("--out", help="output directory")
    diff.add_argument("--threshold", type=float, default=0.0,
                      help="list shots whose changed pixels exceed this percent (default 0)")
    diff.add_argument("--tolerance", type=int, default=0,
                      help="ignore per-pixel differences up to this (0-255, default 0)")
    diff.add_argument("--fail", action="store_true", help="exit 1 when a shot changed")
    args = parser.parse_args()
    return main_shoot(args) if args.cmd == "shoot" else main_diff(args)


if __name__ == "__main__":
    sys.exit(main())
