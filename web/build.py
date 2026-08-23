#!/usr/bin/env python3
"""Build the pecu-cli website into web/_site.

    web/.venv/bin/python web/build.py [--serve]

The site is four generated pages plus a hand-written landing page. The four come
from docs/*.md, which is also what GitHub renders — one source, so a command can
never be documented two different ways.

Links are relative, not rooted, so the output works from a project Pages path
(/pecu-cli/), a user page (/), and a plain file:// open, with no base-URL
setting to get wrong.
"""

import argparse
import hashlib
import html
import json
import os
import re
import shutil
import sys
from pathlib import Path

import markdown

ROOT = Path(__file__).resolve().parent.parent
WEB = ROOT / "web"
DOCS = ROOT / "docs"
MEDIA = DOCS / "media"
OUT = WEB / "_site"

REPO = "https://github.com/chainvue/pecu-cli"
SDK = "https://github.com/chainvue/verus-rust-sdk"
DESCRIPTION = (
    "A command-line Verus wallet. Keys, transparent sends, air-gapped signing, "
    "transaction decoding, VerusIDs and currencies — with no full node to run."
)

# ---------------------------------------------------------------- pages

PAGES = [
    dict(slug="", title="pecu", nav="Home", source=None,
         blurb="A Verus wallet that lives in your terminal."),
    dict(slug="commands", title="Commands", nav="Commands", source="commands.md",
         blurb="Every command, what it prints, and what it refuses to do."),
    dict(slug="configuration", title="Configuration", nav="Configuration",
         source="configuration.md",
         blurb="Flags, environment, config file, and the order they resolve in."),
    dict(slug="design", title="Design notes", nav="Design", source="design.md",
         blurb="Why the output looks the way it does."),
    dict(slug="status", title="Status", nav="Status", source="status.md",
         blurb="What is proven on chain, what is built and waiting, what is out of scope."),
]

# Anchors that used to live in one long document and now live in another.
ANCHOR_MOVES = {"#configuration": "configuration/"}


# ---------------------------------------------------------------- terminal captures

def namespace_svg(text, prefix):
    """Give one svg-term capture its own CSS and id namespace.

    svg-term names things out of a tiny shared pool — classes `a`..`o`, frame ids
    `1`..`n` plus a stray `a`/`b`, and a scroll animation called `r`, `p` or `q`
    depending on nothing the caller controls. Inlining two captures into one
    document therefore cross-wires them silently: the second capture renders in
    the first one's palette, or plays the first one's timeline. Prefixing makes
    each capture self-contained, which is what lets them be inlined at all, which
    is in turn what makes the replay button and the reduced-motion fallback
    possible.

    Everything is renamed, not a hand-listed subset — a capture recorded next
    year will use whatever names svg-term feels like then, and the assertion at
    the end is what turns that from a rendering bug into a build failure.
    """
    keyframes = re.findall(r"@keyframes\s+([\w-]+)\s*\{", text)

    def in_style(match):
        css = match.group(1)
        css = re.sub(r"\.([A-Za-z][\w-]*)", rf".{prefix}\1", css)
        for name in keyframes:
            css = re.sub(rf"@keyframes\s+{re.escape(name)}\s*\{{",
                         f"@keyframes {prefix}k{name}{{", css)
        return f"<style>{css}</style>"

    text = re.sub(r"<style>(.*?)</style>", in_style, text, flags=re.S)
    for name in keyframes:
        text = re.sub(rf"animation-name:\s*{re.escape(name)}\b",
                      f"animation-name:{prefix}k{name}", text)
    text = re.sub(r'class="([^"]+)"',
                  lambda m: 'class="' + " ".join(prefix + c for c in m.group(1).split()) + '"',
                  text)
    text = re.sub(r'id="([^"]+)"', rf'id="{prefix}\1"', text)
    text = re.sub(r'xlink:href="#([^"]+)"', rf'xlink:href="#{prefix}\1"', text)

    leaked = sorted({name for group in re.findall(r'(?:id|class)="([^"]+)"', text)
                     for name in group.split() if not name.startswith(prefix)})
    if leaked:
        raise SystemExit(f"capture kept unprefixed names: {leaked[:6]}")
    return text


def trim_capture(text):
    """Cut the empty terminal off the bottom of a capture.

    `record.sh` is given a row count by hand, and a generous guess costs
    nothing on a recording — the rows are simply never written to. On a page it
    is 350px of blank window between one demo and the next paragraph. The
    deepest glyph in the whole filmstrip is the bottom of the tallest frame, so
    everything below it plus a line of margin is dead space in every frame at
    once, and the nested svg already clips to its own viewport.

    Vertical only: the animation slides the filmstrip sideways, so the width has
    to stay exactly as recorded.
    """
    inner = re.search(r'<svg (?=[^>]*viewBox="0 0 78 )[^>]*>', text)
    root = re.match(r"<svg[^>]*?width=\"([\d.]+)\" height=\"([\d.]+)\"", text)
    if not inner or not root:
        return text
    attrs = inner.group(0)
    box = re.search(r'viewBox="0 0 ([\d.]+) ([\d.]+)"', attrs)
    px = re.search(r'height="([\d.]+)"', attrs)
    top = re.search(r'y="([\d.]+)"', attrs)
    if not (box and px and top):
        return text

    units, px_height, offset = float(box.group(2)), float(px.group(1)), float(top.group(1))
    root_height = float(root.group(2))
    deepest = max([float(y) for y in re.findall(r'<(?:use|text) [^>]*y="([\d.]+)"', text)] or [0])
    if not deepest:
        return text

    keep = deepest + 2.5                       # one line of air under the last glyph
    if keep >= units * 0.92:                   # nothing worth reclaiming
        return text

    scale = px_height / units
    new_px = round(keep * scale, 2)
    new_root = round(root_height - px_height + new_px, 2)

    trimmed = attrs
    trimmed = trimmed.replace(f'viewBox="0 0 {box.group(1)} {box.group(2)}"',
                              f'viewBox="0 0 {box.group(1)} {round(keep, 3)}"')
    trimmed = trimmed.replace(f'height="{px.group(1)}"', f'height="{new_px}"', 1)
    text = text[:inner.start()] + trimmed + text[inner.end():]
    return text.replace(f'height="{root.group(2)}"', f'height="{new_root}"', 1)


def responsive_svg(text):
    """Trade the root svg's fixed pixel size for a viewBox.

    A capture is 820px wide as recorded. Without a viewBox it stays 820px on a
    phone and pushes the page sideways; with one it scales and reserves its own
    height, so the page never reflows once the markup lands.
    """
    match = re.match(r'<svg([^>]*?)width="([\d.]+)" height="([\d.]+)"', text)
    if not match:
        raise SystemExit("capture has no root width/height to convert")
    attrs, width, height = match.groups()
    head = f'<svg{attrs}viewBox="0 0 {width} {height}" width="100%" height="auto"'
    return head + text[match.end():], float(width), float(height)


def close_keyframes(text):
    """Give the filmstrip an explicit last frame.

    svg-term writes `0%`, `17.5%`, `58.7%` and stops. Looping for ever, that
    reads fine — the wrap back to an empty terminal is the next cycle starting.
    Played once it is a trap: with no `100%` stop the browser fills the tail
    against the element's underlying value, so an animation asked to hold its
    final frame holds a blank window instead. Restating the last offset at 100%
    is what makes "stop on the finished screen" mean what it says, and it is
    what the poster frame and the reduced-motion fallback both stand on.
    """
    def fix(match):
        head, body = match.group(1), match.group(2)
        if "100%" in body:
            return match.group(0)
        offsets = [float(x) for x in re.findall(r"translateX\((-?[\d.]+)px\)", body)]
        if not offsets:
            return match.group(0)
        return f"{head}{body}100%{{transform:translateX({min(offsets)}px)}}}}"

    return re.sub(r"(@keyframes\s+[\w-]+\s*\{)((?:[^{}]|\{[^{}]*\})*)\}", fix, text)


def duration(text):
    """How long the capture runs, so it can be parked on its last frame."""
    match = re.search(r"animation-duration:([\d.]+)s", text)
    return float(match.group(1)) if match else 0.0


def end_frame(text):
    """The translateX the filmstrip finishes on.

    Used to park the capture on its last frame for readers who asked for no
    motion — the finished terminal is the useful state, and `animation: none`
    alone would show them frame zero, which is an empty window.
    """
    offsets = [float(x) for x in re.findall(r"translateX\((-?[\d.]+)px\)", text)]
    return min(offsets) if offsets else 0.0


def capture(name, command, caption):
    path = MEDIA / f"{name}-dark.svg"
    if not path.exists():
        raise SystemExit(f"missing capture: {path}")
    # Deterministic: PYTHONHASHSEED is randomised, and a site that rebuilds
    # byte-for-byte is a site whose diffs mean something.
    prefix = "t" + hashlib.sha1(name.encode()).hexdigest()[:6] + "x"
    svg = namespace_svg(path.read_text(), prefix)
    svg = trim_capture(svg)
    svg, _, _ = responsive_svg(svg)
    # Recorded to loop for ever; played once here, holding the finished screen.
    # A capture that restarts under the reader's eyes while they are still
    # reading line four is an animation working against its own content.
    svg = close_keyframes(svg)
    svg = svg.replace("animation-iteration-count:infinite",
                      "animation-iteration-count:1;animation-fill-mode:forwards")
    if "animation-iteration-count:1" not in svg:
        raise SystemExit(f"{name}: capture is not the looping shape this expects")
    return f"""<figure class="term" data-term style="--term-end:{end_frame(svg)}px;--term-dur:{duration(svg)}s">
  <div class="term-screen">{svg}</div>
  <button class="term-replay" type="button" data-replay hidden>
    <svg viewBox="0 0 16 16" aria-hidden="true" width="13" height="13"><path fill="currentColor" d="M8 3V0.5L4.5 3.75 8 7V4.5a3.5 3.5 0 1 1-3.5 3.5H3a5 5 0 1 0 5-5Z"/></svg>
    Replay
  </button>
  <figcaption><code>{html.escape(command)}</code> — {caption}</figcaption>
</figure>"""


# Dark captures on both themes, deliberately. A terminal is a dark surface; a
# screenshot of one keeps its own background the way a photograph does, and the
# frame around it is what tells the eye it is quoted rather than part of the page.
DEMOS = {
    "doctor": ("pecu doctor", "where the files are, what the binary is, whether the node answers"),
    "id": ("pecu id show VRSCTEST@", "authorities, timelock state, and whether the identity can be revoked at all"),
    "tx": ("pecu tx explain &lt;txid&gt;", "a real VRSCTEST currency launch, output by output"),
    "wallet": ("pecu wallet balance", "spendable, withheld and token balances kept apart"),
    "register": ("pecu id register", "both phases of a VerusID registration, resumable in between"),
    "send": ("pecu send --dry-run", "built and signed, and stopped before anything is broadcast"),
    "send-token": ("pecu send --currency", "a token moves; the fee stays in the chain's own coin"),
}


# ---------------------------------------------------------------- markdown

def render(source):
    md = markdown.Markdown(
        extensions=["fenced_code", "tables", "toc", "sane_lists", "attr_list"],
        extension_configs={"toc": {"permalink": "#", "permalink_title": "Link to this section"}},
    )
    body = md.convert(source)
    # `\|` is how a pipe survives a Markdown table cell. GitHub drops the
    # backslash when it renders the cell; Python-Markdown leaves it inside the
    # code span, so `pecu key gen\|import` is what the reader sees.
    body = re.sub(r"<code>([^<]*)</code>",
                  lambda m: "<code>" + m.group(1).replace("\\|", "|") + "</code>", body)
    # A wide table must scroll inside its own box. Letting it widen the page
    # instead moves every paragraph on the page sideways with it.
    body = re.sub(r"<table>", '<div class="table-wrap"><table>', body)
    body = re.sub(r"</table>", "</table></div>", body)
    return body, md.toc_tokens


def flatten_toc(tokens, depth=0, out=None):
    out = [] if out is None else out
    for token in tokens:
        out.append(dict(id=token["id"], name=token["name"], level=token["level"]))
        flatten_toc(token["children"], depth + 1, out)
    return out


def rewrite_links(body, page_slug):
    for old, new in ANCHOR_MOVES.items():
        body = body.replace(f'href="{old}"', f'href="../{new}"')
    # Every off-site link opens in place; only the repo/SDK chrome links get the
    # new-tab treatment, and those are written by hand below.
    return body


# ---------------------------------------------------------------- shell

def nav_links(current, rel):
    items = []
    for page in PAGES:
        href = rel + (f"{page['slug']}/" if page["slug"] else "")
        here = ' aria-current="page"' if page["slug"] == current else ""
        items.append(f'<a href="{href}"{here}>{html.escape(page["nav"])}</a>')
    return "\n        ".join(items)


def shell(*, slug, title, blurb, content, rel, toc=None, source=None, prev=None, nxt=None):
    full_title = "pecu — a Verus wallet in your terminal" if not slug else f"{title} · pecu"

    aside = ""
    if toc is not None:
        entries = []
        for item in toc:
            if item["level"] > 3:
                continue
            cls = f' class="lvl{item["level"]}"'
            entries.append(f'<a{cls} href="#{item["id"]}">{html.escape(item["name"])}</a>')
        aside = f"""<aside class="sidebar">
      <nav class="sidebar-nav" aria-label="Documentation">
        <p class="sidebar-label">Documentation</p>
        {nav_links(slug, rel)}
      </nav>
      <nav class="sidebar-toc" aria-label="On this page">
        <p class="sidebar-label">On this page</p>
        {"".join(entries)}
      </nav>
    </aside>"""

    footer_nav = ""
    if prev or nxt:
        parts = []
        if prev:
            parts.append(f'<a class="pager prev" href="{rel}{prev["slug"]}/"><span>Previous</span>{html.escape(prev["title"])}</a>')
        else:
            parts.append("<span></span>")
        if nxt:
            parts.append(f'<a class="pager next" href="{rel}{nxt["slug"]}/"><span>Next</span>{html.escape(nxt["title"])}</a>')
        footer_nav = f'<nav class="pagers" aria-label="Pagination">{"".join(parts)}</nav>'

    edit = ""
    if source:
        edit = (f'<a class="edit" href="{REPO}/blob/main/docs/{source}">'
                f'Edit this page on GitHub</a>')

    layout = "doc" if toc is not None else "home"

    return f"""<!doctype html>
<html lang="en" class="{layout}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(full_title)}</title>
<meta name="description" content="{html.escape(blurb)}">
<meta property="og:title" content="{html.escape(full_title)}">
<meta property="og:description" content="{html.escape(blurb)}">
<meta property="og:type" content="website">
<meta name="twitter:card" content="summary">
<meta name="color-scheme" content="dark light">
<link rel="icon" href="{rel}assets/favicon.svg" type="image/svg+xml">
<link rel="stylesheet" href="{rel}assets/site.css">
<script>
  // Before first paint: no flash of the wrong theme, and no capture animating
  // before the reader has scrolled to it.
  (function () {{
    var d = document.documentElement;
    d.classList.add('js');
    try {{
      var t = localStorage.getItem('pecu-theme');
      if (t) d.dataset.theme = t;
    }} catch (e) {{}}
  }})();
</script>
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
<header class="topbar">
  <a class="brand" href="{rel}">
    <span class="brand-mark" aria-hidden="true">┌─┐<br>├─┘<br>┴&nbsp;&nbsp;</span>
    <span class="brand-name">pecu</span>
  </a>
  <nav class="topnav" aria-label="Main">
    {nav_links(slug, rel)}
  </nav>
  <div class="topbar-actions">
    <button class="search-open" type="button" data-search-open aria-label="Search documentation">
      <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M11.7 10.3a6 6 0 1 0-1.4 1.4l3.3 3.3 1.4-1.4ZM7 11a4 4 0 1 1 0-8 4 4 0 0 1 0 8Z"/></svg>
      <span>Search</span><kbd>⌘K</kbd>
    </button>
    <button class="icon-btn" type="button" data-theme-toggle aria-label="Switch colour theme">
      <svg class="i-sun" viewBox="0 0 16 16" width="15" height="15" aria-hidden="true"><path fill="currentColor" d="M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm0 2.5v2m0-13v2M13.3 13.3l1.4 1.4M1.3 1.3l1.4 1.4M2.5 8h-2m15 0h-2M2.7 13.3l-1.4 1.4M14.7 1.3l-1.4 1.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
      <svg class="i-moon" viewBox="0 0 16 16" width="15" height="15" aria-hidden="true"><path fill="currentColor" d="M13.5 10.4A5.7 5.7 0 0 1 6 2.6a6 6 0 1 0 7.5 7.8Z"/></svg>
    </button>
    <a class="icon-btn" href="{REPO}" aria-label="Source on GitHub">
      <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path fill="currentColor" d="M8 0a8 8 0 0 0-2.5 15.6c.4.1.5-.2.5-.4v-1.4C3.8 14.2 3.4 13 3.4 13c-.4-.9-.9-1.2-.9-1.2-.7-.5.1-.5.1-.5.8.1 1.2.8 1.2.8.7 1.2 1.9.9 2.4.7 0-.6.3-.9.5-1.1-1.8-.2-3.6-.9-3.6-4 0-.9.3-1.6.8-2.1 0-.2-.3-1 .1-2.1 0 0 .7-.2 2.2.8a7.6 7.6 0 0 1 4 0c1.5-1 2.2-.8 2.2-.8.4 1.1.2 1.9.1 2.1.5.5.8 1.2.8 2.1 0 3.1-1.8 3.8-3.6 4 .3.2.5.7.5 1.5v2.2c0 .2.1.5.6.4A8 8 0 0 0 8 0Z"/></svg>
    </a>
  </div>
</header>
<div class="layout">
  {aside}
  <main id="main" class="content">
{content}
{footer_nav}
{edit}
  </main>
</div>
<footer class="sitefoot">
  <p>Apache-2.0, matching the SDK. The example app for the
     <a href="{SDK}">Verus Rust SDK</a>.</p>
  <p class="foot-warn">Early software. Read <a href="{rel}status/">Status</a> before
     you point it at anything holding real value.</p>
</footer>
<div class="palette" data-palette hidden>
  <div class="palette-scrim" data-palette-close></div>
  <div class="palette-box" role="dialog" aria-modal="true" aria-label="Search documentation">
    <input type="search" data-palette-input placeholder="Search commands, flags, diagnostics…"
           autocomplete="off" spellcheck="false" aria-controls="palette-results">
    <ul id="palette-results" data-palette-results role="listbox"></ul>
    <div class="palette-foot">
      <kbd>↑</kbd><kbd>↓</kbd> navigate <kbd>↵</kbd> open <kbd>esc</kbd> close
    </div>
  </div>
</div>
<script src="{rel}assets/site.js" defer></script>
</body>
</html>
"""


# ---------------------------------------------------------------- search index

WORD = re.compile(r"<[^>]+>")


CHUNK = 900


def chunks(text, size=CHUNK):
    """Cut a long section into overlapping windows.

    One record per heading would be cheaper, but `pecu currency` alone runs to
    450 lines: truncating it to a single window means most of the reference is
    documented on the site and unfindable on it. Windows overlap so a phrase
    that straddles a cut still matches.
    """
    if len(text) <= size:
        return [text]
    out, start = [], 0
    while start < len(text):
        end = min(start + size, len(text))
        if end < len(text):
            space = text.rfind(" ", start + size // 2, end)
            if space > start:
                end = space
        out.append(text[start:end].strip())
        if end >= len(text):
            break
        overlap = max(end - 120, start + 1)
        space = text.find(" ", overlap)
        start = overlap if space < 0 or space - overlap > 30 else space + 1
    return [c for c in out if c]


def sections(body, page_title, page_slug, toc):
    """Split rendered HTML into search records, one or more per heading."""
    records = []
    ids = {item["id"]: item["name"] for item in toc}

    def add(anchor, title, raw):
        text = html.unescape(" ".join(WORD.sub(" ", raw).split()))
        for part in chunks(text):
            records.append(dict(page=page_title, slug=page_slug, anchor=anchor,
                                title=title, text=part))

    pieces = re.split(r'(<h[23][^>]*id="([^"]+)"[^>]*>)', body)
    add("", page_title, pieces[0])
    for i in range(1, len(pieces), 3):
        anchor = pieces[i + 1]
        add(anchor, html.unescape(ids.get(anchor, anchor)), pieces[i + 2])
    return records


# ---------------------------------------------------------------- link check

def check_links(pages_html):
    """Fail the build on a link that goes nowhere.

    A dead link in a reference manual costs a reader more than a missing page
    does: they assume the answer exists and go looking for it.
    """
    ids = {slug: set(re.findall(r'id="([^"]+)"', body)) for slug, body in pages_html.items()}
    slugs = set(pages_html)
    problems = []
    for slug, body in pages_html.items():
        for href in re.findall(r'href="([^"]+)"', body):
            if href.startswith(("http://", "https://", "mailto:")):
                continue
            target, _, anchor = href.partition("#")
            if not target:
                if anchor and anchor not in ids[slug]:
                    problems.append(f"{slug or 'index'}: #{anchor} does not exist")
                continue
            resolved = target.strip("./").rstrip("/")
            if resolved.startswith("assets/") or resolved == "":
                continue
            if resolved not in slugs:
                problems.append(f"{slug or 'index'}: -> {href} has no page")
            elif anchor and anchor not in ids[resolved]:
                problems.append(f"{slug or 'index'}: -> {href} has no such section")
    return problems


NOT_FOUND = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Not found · pecu</title>
<meta name="robots" content="noindex">
<meta name="color-scheme" content="dark light">
<style>
  :root { --bg:#0d1117; --fg:#c9d1d9; --strong:#f0f6fc; --accent:#58a6ff; --green:#3fb950; }
  @media (prefers-color-scheme: light) {
    :root { --bg:#fff; --fg:#1f2328; --strong:#010409; --accent:#0969da; --green:#1a7f37; }
  }
  body { margin:0; min-height:100vh; display:grid; place-items:center; padding:2rem;
         background:var(--bg); color:var(--fg); line-height:1.6;
         font-family:ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif; }
  main { max-width:34rem; }
  pre { margin:0 0 1.5rem; color:var(--green); line-height:1.15;
        font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; font-size:.85rem; }
  h1 { margin:0 0 .6rem; font-size:1.6rem; letter-spacing:-.02em; color:var(--strong); }
  p { margin:0 0 1.4rem; }
  a { color:var(--accent); }
  .keys { display:flex; gap:1.2rem; flex-wrap:wrap; font-size:.95rem; }
</style>
</head>
<body>
<main>
<pre>&#9484;&#9472;&#9488;&#9484;&#9472;&#9488;&#9484;&#9472;&#9488;&#9516; &#9516;
&#9500;&#9472;&#9496;&#9500;&#9508; &#9474;  &#9474; &#9474;
&#9524;  &#9492;&#9472;&#9496;&#9492;&#9472;&#9496;&#9492;&#9472;&#9496;</pre>
<h1>No page at this address.</h1>
<p>Nothing on this site answers to that path. It may have been renamed, or the
   link may have been written by hand.</p>
<p class="keys">
  <a id="home" href="/">Start over</a>
  <a id="docs" href="/">Command reference</a>
  <a href="https://github.com/chainvue/pecu-cli">Source</a>
</p>
</main>
<script>
  var base = location.hostname.slice(-10) === '.github.io'
    ? '/' + location.pathname.split('/')[1] + '/'
    : '/';
  document.getElementById('home').href = base;
  document.getElementById('docs').href = base + 'commands/';
</script>
</body>
</html>
"""


# ---------------------------------------------------------------- main

def build():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)

    index = []
    pages_html = {}
    rendered = {}

    for i, page in enumerate(PAGES):
        slug = page["slug"]
        rel = "" if not slug else "../"
        prev = PAGES[i - 1] if i > 0 else None
        nxt = PAGES[i + 1] if i + 1 < len(PAGES) else None

        if page["source"] is None:
            body = (WEB / "landing.html").read_text()
            for name, (command, caption) in DEMOS.items():
                token = f"<!--demo:{name}-->"
                if token not in body:
                    raise SystemExit(f"landing.html never places the {name} capture")
                body = body.replace(token, capture(name, command, caption))
            if "<!--demo:" in body:
                raise SystemExit("landing.html asks for a capture that does not exist")
            toc = None
        else:
            source = (DOCS / page["source"]).read_text()
            body, toc_tokens = render(source)
            toc = flatten_toc(toc_tokens)
            body = rewrite_links(body, slug)
            index.extend(sections(body, page["title"], slug, toc))
            body = f'<div class="prose">{body}</div>'

        rendered[slug] = dict(page=page, body=body, rel=rel, toc=toc, prev=prev, nxt=nxt)
        pages_html[slug] = body

    problems = check_links(pages_html)
    if problems:
        for line in problems:
            print(f"  broken link: {line}", file=sys.stderr)
        raise SystemExit(f"{len(problems)} broken internal link(s)")

    for slug, data in rendered.items():
        page = data["page"]
        out = shell(slug=slug, title=page["title"], blurb=page["blurb"],
                    content=data["body"], rel=data["rel"], toc=data["toc"],
                    source=page["source"], prev=data["prev"], nxt=data["nxt"])
        target = OUT / slug / "index.html" if slug else OUT / "index.html"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(out)

    # A 404 is served for any missing path, at any depth, so it cannot use the
    # relative asset links every other page relies on. It carries its own styles
    # and works out the site root itself — `/` for a user page, `/<repo>/` for a
    # project one.
    (OUT / "404.html").write_text(NOT_FOUND)

    assets = OUT / "assets"
    assets.mkdir()
    for name in ("site.css", "site.js", "favicon.svg"):
        shutil.copy(WEB / "assets" / name, assets / name)
    (assets / "search.json").write_text(json.dumps(index, separators=(",", ":")))
    # Pages runs Jekyll over anything it is handed unless told not to; a folder
    # starting with an underscore would quietly vanish.
    (OUT / ".nojekyll").write_text("")

    total = sum(f.stat().st_size for f in OUT.rglob("*") if f.is_file())
    print(f"built {len(rendered)} pages, {len(index)} search records, {total // 1024} KB")
    return OUT


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--serve", action="store_true", help="serve on :8000 afterwards")
    args = parser.parse_args()
    out = build()
    if args.serve:
        os.chdir(out)
        import http.server
        print("http://localhost:8000")
        http.server.test(HandlerClass=http.server.SimpleHTTPRequestHandler, port=8000, bind="127.0.0.1")
