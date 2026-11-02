#!/usr/bin/env python3
"""Generate the keybinding and settings reference fragments from the rsimgv source.

Usage: extract_reference.py [PATH_TO_RSIMGV]   (default: ../rsimgv)

Reads src/keybindings/actions.rs, src/settings/store.rs and data/au.topher.rimgv.gschema.xml and writes
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


def contexts(name):
    home = {"browse": "Browsing", "view": "Viewer", "image": "Picture", "video": "Video"}.get(name.split(".")[0], "Global")
    if name in ("browse.toggle-hidden", "browse.toggle-media-only"):
        return home, "any context"
    if home == "Global":
        return home, "any context"
    if home == "Browsing":
        return home, "Browsing, Global"
    if home == "Viewer":
        return home, "Viewer, Picture, Video, Global"
    return home, f"{home}, Global"


def pretty(chord):
    names = {"comma": ",", "period": ".", "equal": "=", "minus": "-", "return": "Enter", "escape": "Esc",
             "page_up": "Page Up", "page_down": "Page Down", "backspace": "Backspace", "space": "Space"}
    parts = chord.split("+")
    shown = [names.get(p, p.upper() if len(p) <= 2 else p.capitalize()) for p in parts]
    return "<kbd>" + "</kbd>+<kbd>".join(esc(p) for p in shown) + "</kbd>"


def write_keybindings():
    order = ["Global", "Browsing", "Viewer", "Picture", "Video"]
    blurbs = {
        "Global": "Work in every context unless a more specific one binds the same key.",
        "Browsing": "Active while you browse a folder.",
        "Viewer": "Active in the viewer, for pictures and videos alike.",
        "Picture": "Active while the viewer shows a picture.",
        "Video": "Active while the viewer shows a video.",
    }
    grouped = {k: [] for k in order}
    for name, desc, label, keys in actions():
        grouped[contexts(name)[0]].append((name, desc, label, keys))
    parts = []
    for group in order:
        parts.append(f'<h3 id="keys-{group.lower()}">{group}</h3>\n<p>{blurbs[group]}</p>')
        parts.append('<div class="table-wrap"><table>\n<thead><tr><th>Action</th><th>Default keys</th><th>What it does</th><th>Can be bound in</th></tr></thead>\n<tbody>')
        for name, desc, label, keys in grouped[group]:
            keycell = " ".join(pretty(k) for k in keys) or '<span class="muted">unbound</span>'
            parts.append(
                f'<tr><td><strong>{esc(label)}</strong><br><code>{esc(name)}</code></td><td class="keys">{keycell}</td>'
                f"<td>{esc(desc)}</td><td>{contexts(name)[1]}</td></tr>"
            )
        parts.append("</tbody></table></div>")
    (out / "_keybindings.html").write_text("\n".join(parts) + "\n")
    return sum(len(v) for v in grouped.values())


def layout():
    src = (root / "src/settings/store.rs").read_text()
    block = src[src.index("const LAYOUT"):]
    block = block[: block.index("];")]
    return {key: (group, name) for key, group, name in re.findall(r'\("([^"]+)", "([^"]+)", "([^"]+)"\)', block)}


SECTIONS = [
    ("Window and start-up", ["window-width", "window-height", "window-maximized", "start-location", "start-directory", "last-directory", "last-file", "sidebar-width", "kde-effects"]),
    ("Browsing", ["show-hidden", "media-only", "item-size", "filename-display", "show-directories", "info-layout", "info-fields", "thumbnail-crop", "collapse-on-navigate", "sort-by", "sort-order", "remember-folder-sort", "folder-end", "directory-previews", "folder-preview-count", "folder-preview-fit", "folder-preview-cycle", "video-frame-previews"]),
    ("Viewer", ["open-fullscreen", "auto-hide-cursor", "smooth-scrolling", "image-info-in-title", "zoom-indicator", "viewer-status-bar", "viewer-status-overlay-position", "viewer-blurred-background", "viewer-thumbnail-bar", "viewer-thumbnail-bar-shown", "viewer-thumbnail-bar-overlay", "viewer-thumbnail-bar-pinned", "viewer-thumbnail-bar-size"]),
    ("Pictures", ["image-fit", "keep-fit-mode", "original-focus", "unlock-minimum-zoom", "zoom-step", "expand-up-to-factor", "smooth-upscaling", "scaling-filter", "image-preloading"]),
    ("Videos", ["video-backend", "mpv-args", "seek-overlay", "seek-overlay-bar", "seek-overlay-time", "seek-overlay-time-format", "seek-overlay-position", "seek-overlay-seconds", "seek-overlay-width", "seek-overlay-always"]),
    ("SVP", ["svp-enabled", "svp-plugin-directory", "svp-fps-mode", "svp-fps-target", "svp-fps-minimum", "svp-fps-maximum", "svp-nvof"]),
    ("Mouse and scrolling", ["mouse-back", "mouse-forward", "scroll-image", "scroll-video", "scroll-speed"]),
    ("Thumbnail generator", ["thumbnail-threads", "thumbnail-cache", "thumbnail-unload-offscreen"]),
    ("Remote control", ["ipc-socket", "ipc-socket-path", "ipc-tcp-enabled", "ipc-tcp-port", "ipc-http-enabled", "ipc-http-port", "ipc-external-access"]),
    ("Scripts", ["scripts"]),
]


def write_settings():
    tree = ET.parse(root / "data/au.topher.rimgv.gschema.xml").getroot()
    keys = {}
    for key in tree.iter("key"):
        choices = [c.get("value") for c in key.iter("choice")]
        rng = key.find("range")
        keys[key.get("name")] = dict(
            type=key.get("type"),
            default=(key.findtext("default") or "").strip("'"),
            summary=(key.findtext("summary") or "").strip(),
            choices=choices,
            range=(rng.get("min"), rng.get("max")) if rng is not None else None,
        )
    lay = layout()
    listed = {k for _, ks in SECTIONS for k in ks}
    missing = sorted(set(keys) - listed)
    if missing:
        print("warning: settings not placed in a section:", ", ".join(missing), file=sys.stderr)
    parts = []
    for title, names in SECTIONS:
        slug = re.sub(r"[^a-z]+", "-", title.lower()).strip("-")
        parts.append(f'<h2 id="{slug}">{title}</h2>\n<div class="table-wrap"><table>\n<thead><tr><th>Setting</th><th>Default</th><th>Values</th><th>What it does</th></tr></thead>\n<tbody>')
        for name in names:
            info = keys.get(name)
            if not info:
                print("warning: no such key", name, file=sys.stderr)
                continue
            group, yname = lay.get(name, ("settings", name))
            if info["choices"]:
                values = ", ".join(f"<code>{esc(c)}</code>" for c in info["choices"])
            elif info["range"]:
                values = f"{info['range'][0]} to {info['range'][1]}"
            else:
                values = {"b": "on or off", "i": "whole number", "d": "number", "s": "text", "as": "list", "a{ss}": "list"}.get(info["type"], info["type"])
            default = info["default"]
            if info["type"] == "b":
                default = "on" if default == "true" else "off"
            default = f"<code>{esc(default)}</code>" if default not in ("on", "off") else default
            if default == "<code></code>":
                default = '<span class="muted">empty</span>'
            parts.append(
                f'<tr><td><code>{esc(group)}.{esc(yname)}</code><br><span class="muted">{esc(name)}</span></td><td>{default}</td><td>{values}</td><td>{esc(info["summary"])}</td></tr>'
            )
        parts.append("</tbody></table></div>")
    (out / "_settings.html").write_text("\n".join(parts) + "\n")
    return len(keys)


if __name__ == "__main__":
    print(f"{write_keybindings()} actions, {write_settings()} settings")
