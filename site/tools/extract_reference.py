#!/usr/bin/env python3
"""Generate the keybinding and settings reference fragments from the rsimgv source.

Usage: extract_reference.py [PATH_TO_RSIMGV]   (default: ../rsimgv)

Reads src/keybindings/actions.rs and data/au.topher.rsimgv.gschema.xml and writes
content/_keybindings.html and content/_settings.html, which the documentation pages include.
"""

import html
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

root = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent.parent / "rsimgv")
out = Path(__file__).resolve().parent.parent / "content"
esc = html.escape


def rust_str(text):
    return text.replace('\\"', '"').replace("\\'", "'")


def actions():
    src = (root / "src/keybindings/actions.rs").read_text()
    table = src[src.index("pub const ACTIONS"):]
    pattern = re.compile(
        r'ActionInfo \{\s*name: ([^,]+), description: "((?:[^"\\]|\\.)*)", icon: "[^"]*", label: "([^"]*)", defaults: &\[([^\]]*)\],'
    )
    consts = {"NOTIFY_ACTION": "win.notify", "SCRIPT_ACTION": "script"}
    result = []
    for name, desc, label, defaults in pattern.findall(table):
        name = name.strip()
        name = consts.get(name, name.strip('"'))
        keys = re.findall(r'"([^"]+)"', defaults)
        result.append((name, rust_str(desc), label, keys))
    return result


# A context and how the editor names it, in the order Scope::ALL lists them.
SCOPES = [
    ("global", "Global"),
    ("browse", "Browsing"),
    ("view", "Viewer"),
    ("image", "Picture"),
    ("video", "Video"),
    ("playlist", "Playlist"),
    ("tree", "Folder Tree"),
]
SCOPE_TITLES = dict(SCOPES)


def home_scope(name):
    """The scope an action belongs to, from the prefix of its name."""
    prefix = name.split(".")[0]
    return {"browse": "browse", "view": "view", "image": "image", "video": "video", "playlist": "playlist", "tree": "tree"}.get(prefix, "global")


def contexts(name):
    """The contexts an action can be bound in, following ActionInfo::contexts in the source."""
    home = home_scope(name)
    if name in ("browse.toggle-hidden", "browse.toggle-media-only") or home == "global":
        return [scope for scope, _ in SCOPES]
    if home == "browse":
        return ["global", "browse"]
    if home == "view":
        return ["global", "view", "image", "video"]
    if home == "image":
        return ["global", "image"]
    if home == "video":
        return ["global", "video"]
    if home == "playlist":
        return ["global", "playlist"]
    if home == "tree":
        return ["global", "tree"]
    return ["global"]


def pretty(chord):
    names = {
        "comma": ",", "period": ".", "equal": "=", "minus": "-", "return": "Enter", "escape": "Esc",
        "page_up": "Page Up", "page_down": "Page Down", "backspace": "Backspace", "space": "Space",
        "bracketleft": "[", "bracketright": "]", "up": "Up", "down": "Down", "left": "Left",
        "right": "Right", "home": "Home", "end": "End", "insert": "Insert", "delete": "Delete",
        "tab": "Tab",
    }
    parts = chord.split("+")
    shown = []
    for part in parts:
        if part in names:
            shown.append(names[part])
        elif re.fullmatch(r"f\d{1,2}", part):
            shown.append(part.upper())
        elif len(part) == 1:
            shown.append(part.upper())
        else:
            shown.append(part.replace("_", " ").capitalize())
    return "<kbd>" + "</kbd>+<kbd>".join(esc(p) for p in shown) + "</kbd>"


def write_keybindings():
    order = [scope for scope, _ in SCOPES]
    blurbs = {
        "global": "Work in every context unless a more specific one binds the same key.",
        "browse": "Active while you browse a folder.",
        "view": "Active in the viewer, for pictures and videos alike.",
        "image": "Active while the viewer shows a picture.",
        "video": "Active while the viewer shows a video.",
        "playlist": "Active while the playlist has the focus, on top of browsing or the viewer.",
        "tree": "Active while the folder tree has the focus, on top of browsing or the viewer.",
    }
    grouped = {k: [] for k in order}
    for name, desc, label, keys in actions():
        grouped[home_scope(name)].append((name, desc, label, keys))
    parts = []
    for group in order:
        title = SCOPE_TITLES[group]
        parts.append(f'<h3 id="keys-{group}">{title}</h3>\n<p>{blurbs[group]}</p>')
        parts.append('<div class="table-wrap"><table>\n<thead><tr><th>Action</th><th>Default keys</th><th>What it does</th><th>Can be bound in</th></tr></thead>\n<tbody>')
        for name, desc, label, keys in grouped[group]:
            keycell = " ".join(pretty(k) for k in keys) or '<span class="muted">unbound</span>'
            where = ", ".join(SCOPE_TITLES[scope] for scope in contexts(name))
            parts.append(
                f'<tr><td><strong>{esc(label)}</strong><br><code>{esc(name)}</code></td><td class="keys">{keycell}</td>'
                f"<td>{esc(desc)}</td><td>{where}</td></tr>"
            )
        parts.append("</tbody></table></div>")
    (out / "_keybindings.html").write_text("\n".join(parts) + "\n")
    return sum(len(v) for v in grouped.values())


# The settings, grouped as the documentation covers them. Every key the schema has must be named here, so a
# setting added to the application shows up as a warning rather than silently going undocumented.
SECTIONS = [
    ("Window and start-up", [
        "window.width", "window.height", "window.maximized",
        "window.sidebar.width", "window.sidebar.bookmarks-expanded", "window.sidebar.filesystem-expanded",
        "window.sidebar.animations", "window.sidebar.animation-ms", "window.chat-width",
        "start.location", "start.directory", "start.last-directory", "start.last-file",
    ]),
    ("Browsing", [
        "browse.show-hidden", "browse.media-only", "browse.show-images", "browse.show-videos",
        "browse.show-directories", "browse.collapse-on-navigate", "browse.item-size",
        "browse.filename-display", "browse.video-frame-previews",
        "browse.info.layout", "browse.info.fields",
        "browse.sort.by", "browse.sort.order", "browse.sort.remember-per-folder",
        "browse.folder-previews.enabled", "browse.folder-previews.count", "browse.folder-previews.fit",
        "browse.folder-previews.cycle", "browse.tree-monitoring",
    ]),
    ("Thumbnails", [
        "thumbnails.threads", "thumbnails.cache", "thumbnails.aggression", "thumbnails.unload-offscreen",
        "thumbnails.max-image-size", "thumbnails.max-video-size", "thumbnails.crop", "thumbnails.crop-video",
    ]),
    ("Viewer", [
        "viewer.open-fullscreen", "viewer.auto-hide-cursor", "viewer.image-info-in-title",
        "viewer.blurred-background", "viewer.overlay-layout", "viewer.folder-end",
        "viewer.slideshow-delay", "viewer.slideshow-shuffle",
        "viewer.status-bar.shown", "viewer.status-bar.overlay-position",
        "viewer.thumbnail-bar.side", "viewer.thumbnail-bar.shown", "viewer.thumbnail-bar.overlay",
        "viewer.thumbnail-bar.pinned", "viewer.thumbnail-bar.size",
    ]),
    ("Pictures", [
        "image.fit", "image.keep-fit", "image.original-focus", "image.unlock-minimum-zoom",
        "image.zoom-step", "image.expand-up-to-factor", "image.zoom-indicator", "image.smooth-scrolling",
        "image.scaling-filter", "image.smooth-upscaling", "image.preloading",
    ]),
    ("Video", [
        "video.backend", "video.start-delay", "video.scaled-times", "video.app-volume",
        "video.mpv.args", "video.mpv.opengl",
        "video.crop.enabled", "video.crop.method", "video.crop.ratio", "video.crop.trim",
        "video.backdrop.enabled", "video.backdrop.zoom", "video.backdrop.blur", "video.backdrop.dim",
        "video.controls.locked", "video.controls.x", "video.controls.y", "video.controls.width",
    ]),
    ("Seek overlay (older layout)", [
        "video.seek-overlay.enabled", "video.seek-overlay.bar", "video.seek-overlay.time",
        "video.seek-overlay.time-format", "video.seek-overlay.position", "video.seek-overlay.seconds",
        "video.seek-overlay.width", "video.seek-overlay.always",
    ]),
    ("SVP", [
        "video.svp.enabled", "video.svp.nvof", "video.svp.plugin-directory",
        "video.svp.fps.mode", "video.svp.fps.target", "video.svp.fps.minimum", "video.svp.fps.maximum",
        "video.svp.seek-bypass.enabled", "video.svp.seek-bypass.delay",
    ]),
    ("Remembered file settings", [
        "remember.enabled", "remember.position", "remember.speed", "remember.volume", "remember.aspect",
        "remember.rotation", "remember.zoom", "remember.pan",
    ]),
    ("Viewing history", [
        "history.enabled", "history.images", "history.image-seconds", "history.videos",
        "history.video-seconds", "history.video-percent", "history.count-paused", "history.show-stats",
    ]),
    ("Playlist", [
        "playlist.fetch-in-background", "playlist.repeat", "playlist.shown", "playlist.width",
        "playlist.detached", "playlist.sort",
        "playlist.columns.position", "playlist.columns.name", "playlist.columns.length",
        "playlist.columns.filename", "playlist.columns.widths",
        "playlist.window.width", "playlist.window.height", "playlist.window.headerbar",
    ]),
    ("Mouse and scrolling", [
        "mouse.back", "mouse.forward", "mouse.scroll.image", "mouse.scroll.video", "mouse.scroll.speed",
    ]),
    ("Remote control", [
        "remote.socket.enabled", "remote.socket.path", "remote.external-access",
        "remote.tcp.enabled", "remote.tcp.port", "remote.http.enabled", "remote.http.port",
    ]),
    ("Scripts", ["scripts.list"]),
    ("Plugins", ["plugins.disabled", "plugins.extra-directories", "plugins.auto-reload"]),
]


def schema_settings():
    """Every key in the schema by its dotted name, with the group it is written under in the settings file."""
    tree = ET.parse(root / "data/au.topher.rsimgv.gschema.xml").getroot()
    root_path = tree.find("schema").get("path")
    result = {}
    for schema in tree.iter("schema"):
        path = schema.get("path")
        group = path.removeprefix(root_path).strip("/").replace("/", ".")
        for key in schema.findall("key"):
            choices = [c.get("value") for c in key.iter("choice")]
            rng = key.find("range")
            name = f"{group}.{key.get('name')}" if group else key.get("name")
            result[name] = dict(
                group=group or "settings",
                key=key.get("name"),
                type=key.get("type"),
                default=(key.findtext("default") or "").strip("'"),
                summary=(key.findtext("summary") or "").strip(),
                choices=choices,
                range=(rng.get("min"), rng.get("max")) if rng is not None else None,
            )
    return result


def write_settings():
    keys = schema_settings()
    listed = {name for _, names in SECTIONS for name in names}
    missing = sorted(set(keys) - listed)
    unknown = sorted(listed - set(keys))
    if missing:
        print("warning: settings not placed in a section:", ", ".join(missing), file=sys.stderr)
    if unknown:
        print("warning: sections name settings that do not exist:", ", ".join(unknown), file=sys.stderr)
    parts = []
    for title, names in SECTIONS:
        slug = re.sub(r"[^a-z]+", "-", title.lower()).strip("-")
        parts.append(f'<h2 id="{slug}">{title}</h2>\n<div class="table-wrap"><table>\n<thead><tr><th>Setting</th><th>Default</th><th>Values</th><th>What it does</th></tr></thead>\n<tbody>')
        for name in names:
            info = keys.get(name)
            if not info:
                continue
            if info["choices"]:
                values = ", ".join(f"<code>{esc(c)}</code>" for c in info["choices"])
            elif info["range"]:
                values = f"{info['range'][0]} to {info['range'][1]}"
            else:
                values = {"b": "on or off", "i": "whole number", "d": "number", "s": "text", "as": "list", "ai": "list", "(sb)": "text and on/off"}.get(info["type"], esc(info["type"]))
            default = info["default"]
            if info["type"] == "b":
                default = "on" if default == "true" else "off"
            default = f"<code>{esc(default)}</code>" if default not in ("on", "off") else default
            if default == "<code></code>":
                default = '<span class="muted">empty</span>'
            dotted = f"{info['group']}.{info['key']}"
            parts.append(
                f'<tr><td><code>{esc(dotted)}</code><br><span class="muted">{esc(info["key"])}</span></td><td>{default}</td><td>{values}</td><td>{esc(info["summary"])}</td></tr>'
            )
        parts.append("</tbody></table></div>")
    (out / "_settings.html").write_text("\n".join(parts) + "\n")
    return len(keys)


if __name__ == "__main__":
    print(f"{write_keybindings()} actions, {write_settings()} settings")
