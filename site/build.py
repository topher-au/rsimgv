#!/usr/bin/env python3
"""Build the rsimgv website: content/*.html fragments wrapped in the site layout, written to public/.

Usage: build.py            build into public/
       build.py --serve    build, then serve public/ on http://localhost:8000

A page is a file with `key: value` header lines, a line holding `---`, then HTML. Keys: title, desc, layout
(`home`, `page` or `doc`; docs/ pages default to `doc`). Shortcodes in the body:

  {{include _file.html}}         the fragment content/_file.html
  {{shot name "Caption"}}        a figure of assets/screenshots/name.webp that opens in a lightbox
  {{root}}                       the path from this page back to the site root
"""

import html
import json
import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CONTENT = ROOT / "content"
OUT = ROOT / "public"
SITE = "rsimgv"
REPO = "https://git.topher.au/topher/rsimgv"
MIRROR = "https://github.com/topher-au/rsimgv"

# The documentation, in reading order: (slug, sidebar title) grouped under a heading.
DOCS = [
    ("Start here", [
        ("index", "Overview"),
        ("getting-started", "Getting started"),
        ("command-line", "Command line"),
    ]),
    ("Using rsimgv", [
        ("browsing", "Browsing and searching"),
        ("navigation", "Navigation, tabs and bookmarks"),
        ("viewer", "The picture viewer"),
        ("video", "Playing video"),
        ("playlist", "The playlist"),
        ("svp", "SVP frame interpolation"),
        ("history", "Viewing history"),
    ]),
    ("Customising", [
        ("keybindings", "Keybindings and mouse"),
        ("overlay", "Overlays and notifications"),
        ("scripts", "Scripts and messages"),
        ("plugins", "Plugins"),
        ("settings", "Settings reference"),
        ("files", "Files and folders"),
    ]),
    ("Automation", [
        ("remote-control", "Remote control (IPC)"),
    ]),
    ("Project", [
        ("development", "Development"),
    ]),
]
DOC_ORDER = [(slug, title) for _, items in DOCS for slug, title in items]


def parse(path):
    text = path.read_text()
    head, sep, body = text.partition("\n---\n")
    meta = {}
    if sep:
        for line in head.splitlines():
            key, _, value = line.partition(":")
            meta[key.strip()] = value.strip()
    else:
        body = text
    return meta, body


def expand(body, root):
    def include(match):
        return (CONTENT / match.group(1)).read_text()

    def shot(match):
        name, caption = match.group(1), match.group(2)
        src = f"{root}assets/screenshots/{name}.webp"
        return (
            f'<figure class="shot"><a href="{src}" data-lightbox><img src="{src}" width="1280" height="800" '
            f'loading="lazy" alt="{html.escape(caption, quote=True)}"></a><figcaption>{caption}</figcaption></figure>'
        )

    body = re.sub(r"\{\{include ([\w./-]+)\}\}", include, body)
    body = re.sub(r'\{\{shot ([\w-]+) "([^"]*)"\}\}', shot, body)
    return body.replace("{{root}}", root)


def add_ids(body):
    """Gives every h2 and h3 an id, and returns the new body with the table of contents entries."""
    toc, used = [], set()

    def slug(text):
        base = re.sub(r"[^a-z0-9]+", "-", re.sub(r"<[^>]+>", "", html.unescape(text)).lower()).strip("-") or "section"
        candidate, n = base, 2
        while candidate in used:
            candidate, n = f"{base}-{n}", n + 1
        used.add(candidate)
        return candidate

    def heading(match):
        level, attrs, text = match.group(1), match.group(2), match.group(3)
        existing = re.search(r'id="([^"]+)"', attrs)
        ident = existing.group(1) if existing else slug(text)
        used.add(ident)
        toc.append((int(level), ident, re.sub(r"<[^>]+>", "", text)))
        anchor = f'<a class="anchor" href="#{ident}" aria-label="Link to this section">#</a>'
        return f'<h{level}{attrs if existing else attrs + f" id={chr(34)}{ident}{chr(34)}"}>{text}{anchor}</h{level}>'

    return re.sub(r"<h([23])([^>]*)>(.*?)</h\1>", heading, body, flags=re.S), toc


def nav_links(root, current):
    links = [("Features", "features.html", "features"), ("Docs", "docs/index.html", "docs"), ("Download", "download.html", "download")]
    out = []
    for label, href, key in links:
        cls = ' aria-current="page"' if key == current else ""
        out.append(f'<a href="{root}{href}"{cls}>{label}</a>')
    return "\n".join(out)


def layout(meta, body, root, current, extra_head="", sidebar="", toc_html="", pager=""):
    title = meta.get("title", SITE)
    full_title = SITE if title == SITE else f"{title} · {SITE}"
    desc = html.escape(meta.get("desc", "An integrated file explorer and media viewer for GNOME."), quote=True)
    docs_class = " has-docs" if sidebar else ""
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(full_title)}</title>
<meta name="description" content="{desc}">
<meta property="og:title" content="{html.escape(full_title, quote=True)}">
<meta property="og:description" content="{desc}">
<meta property="og:image" content="{root}assets/screenshots/viewer-image.webp">
<meta name="theme-color" content="#1e1e20">
<link rel="icon" type="image/png" href="{root}assets/favicon.png">
<link rel="stylesheet" href="{root}static/style.css">
<script>try{{var t=localStorage.getItem("theme");if(t)document.documentElement.dataset.theme=t}}catch(e){{}}</script>
{extra_head}
</head>
<body class="{current}{docs_class}">
<a class="skip" href="#main">Skip to content</a>
<header class="site-header">
  <div class="wrap bar">
    <a class="brand" href="{root}index.html"><img src="{root}assets/icon.png" width="32" height="32" alt="">rsimgv</a>
    <button class="menu-toggle" aria-label="Menu" aria-expanded="false" aria-controls="site-nav">☰</button>
    <nav id="site-nav" aria-label="Main">
{nav_links(root, current)}
      <a href="{REPO}">Source</a>
      <button class="theme-toggle" type="button" aria-label="Switch between light and dark">◐</button>
    </nav>
  </div>
</header>
{sidebar}
<main id="main">
{body}
{pager}
</main>
{toc_html}
<footer class="site-footer">
  <div class="wrap">
    <p>rsimgv is free software, released under the <a href="{REPO}/src/branch/main/LICENSE">GPL-3.0-or-later</a> licence.
    Source on <a href="{REPO}">git.topher.au</a> · mirror on <a href="{MIRROR}">GitHub</a>.</p>
    <p class="muted">Screenshots show synthetic demo pictures and clips, not personal files.</p>
  </div>
</footer>
<div class="lightbox" hidden><img alt=""><button type="button" aria-label="Close">×</button></div>
<script src="{root}static/site.js" defer></script>
</body>
</html>
"""


def build():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    shutil.copytree(ROOT / "assets", OUT / "assets")
    shutil.copytree(ROOT / "static", OUT / "static")
    search = []

    pages = [p for p in CONTENT.rglob("*.html") if not p.name.startswith("_")]
    for path in sorted(pages):
        rel = path.relative_to(CONTENT)
        depth = len(rel.parts) - 1
        root = "../" * depth
        meta, body = parse(path)
        body = expand(body, root)
        is_doc = rel.parts[0] == "docs"
        kind = meta.get("layout", "doc" if is_doc else "page")
        current = "docs" if is_doc else rel.stem
        slug = rel.stem
        sidebar = toc_html = pager = ""

        if kind == "doc":
            body, toc = add_ids(body)
            side = ['<aside class="doc-nav" aria-label="Documentation"><div class="search"><input type="search" id="doc-search" placeholder="Search the docs" aria-label="Search the docs" autocomplete="off"><ul id="search-results" hidden></ul></div>']
            for group, items in DOCS:
                side.append(f"<h2>{group}</h2><ul>")
                for s, t in items:
                    cur = ' aria-current="page"' if s == slug else ""
                    side.append(f'<li><a href="{root}docs/{s}.html"{cur}>{t}</a></li>')
                side.append("</ul>")
            side.append("</aside>")
            sidebar = "\n".join(side)
            entries = [(l, i, t) for l, i, t in toc if l == 2 or (l == 3 and len(toc) < 25)]
            if len(entries) > 1:
                items = "".join(f'<li class="l{l}"><a href="#{i}">{html.escape(t)}</a></li>' for l, i, t in entries)
                toc_html = f'<nav class="toc" aria-label="On this page"><h2>On this page</h2><ul>{items}</ul></nav>'
            order = [s for s, _ in DOC_ORDER]
            if slug in order:
                k = order.index(slug)
                prev = DOC_ORDER[k - 1] if k > 0 else None
                nxt = DOC_ORDER[k + 1] if k + 1 < len(DOC_ORDER) else None
                parts = []
                if prev:
                    parts.append(f'<a class="prev" href="{prev[0]}.html"><small>Previous</small>{prev[1]}</a>')
                if nxt:
                    parts.append(f'<a class="next" href="{nxt[0]}.html"><small>Next</small>{nxt[1]}</a>')
                pager = f'<nav class="pager" aria-label="Pages">{"".join(parts)}</nav>'
            body = f'<article class="doc"><h1>{meta.get("title", "")}</h1>\n{body}</article>'
            text = re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", body)))
            search.append({"t": meta.get("title", slug), "u": f"docs/{slug}.html", "h": [t for l, i, t in toc], "x": text[:6000]})
        elif kind == "page":
            wide = " wide" if meta.get("wide") else ""
            body = f'<div class="wrap page{wide}">{body}</div>'

        out = OUT / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(layout(meta, body, root, current, sidebar=sidebar, toc_html=toc_html, pager=pager))

    (OUT / "search-index.json").write_text(json.dumps(search, separators=(",", ":")))
    (OUT / ".nojekyll").write_text("")
    print(f"built {len(pages)} pages into {OUT}")


if __name__ == "__main__":
    build()
    if "--serve" in sys.argv:
        import functools
        import http.server

        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(OUT))
        print("serving on http://localhost:8000")
        http.server.ThreadingHTTPServer(("", 8000), handler).serve_forever()
