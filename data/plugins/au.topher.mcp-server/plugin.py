"""MCP Server: control rsimgv from MCP clients.

The plugin serves the Model Context Protocol (streamable HTTP transport, JSON
responses). Tools are small handlers over the host methods the IPC socket offers
and over the keybound actions. HTTP requests are handled on worker threads; every
host call is queued to the GTK main thread with ``rsimgv.ui.idle_add``.

The tool list is catalogued, with what is left out and why, in TOOLS.md.
"""

import hmac
import ipaddress
import json
import mimetypes
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import rsimgv

PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
CALL_TIMEOUT = 130  # a little over the host's 120 s wait for plugin actions
MAX_BODY = 1_000_000
MAX_THUMBNAILS = 12
MAX_METADATA_PATHS = 50
MAX_STEPS = 20
LOCAL_HOSTS = {"127.0.0.1", "localhost", "::1"}
DEFAULTS = {"port": 8765, "bind_address": "127.0.0.1", "token": "", "read_only": False,
            "allow_raw_methods": False}


class _ToolError(Exception):
    """A failure the model should see as a tool result, not a protocol error."""


# Actions and settings that stay out of reach of MCP clients; see TOOLS.md for the reasons.
EXCLUDED_ACTIONS = frozenset({
    "app.quit", "win.notify",
    # Dialogs that wait for a person at the desktop.
    "win.settings", "win.settings-keybindings", "win.open-folder", "win.edit-location", "win.bookmark-folder",
    "win.playlist-load", "win.playlist-save", "win.playlist-clear", "browse.properties", "win.toggle-keybinds",
    # Need a pointer or a drag.
    "image.zoom-in-at-pointer", "image.zoom-out-at-pointer", "browse.show-tooltip", "video.edit-crop",
    # Keyboard cursor and marking, which depend on focus; select_file is the reliable route.
    "browse.first", "browse.last", "browse.select-all", "browse.toggle-mark", "browse.toggle-search",
    # Layout chrome.
    "win.toggle-sidebar", "win.toggle-filesystem", "win.toggle-bookmarks", "win.toggle-playlist",
    "win.playlist-detach", "view.pin", "view.toggle-thumbnail-bar", "view.toggle-thumbnail-bar-pin",
    "view.toggle-thumbnail-bar-overlay", "view.toggle-status-bar",
})
EXCLUDED_ACTION_PREFIXES = ("browse.cursor-", "browse.keep-", "browse.extend-", "tree.", "playlist.", "win.focus-")
EXCLUDED_SETTING_PREFIXES = ("plugins.",)


def _action_excluded(name):
    return name in EXCLUDED_ACTIONS or name.startswith(EXCLUDED_ACTION_PREFIXES)


def _setting_excluded(key):
    return str(key).startswith(EXCLUDED_SETTING_PREFIXES)


# Tool registry ---------------------------------------------------------------

TOOLS = {}


class Tool:
    def __init__(self, name, title, description, schema, handler, read_only, destructive, idempotent, raw):
        self.name = name
        self.title = title
        self.description = description
        self.schema = schema
        self.handler = handler
        self.read_only = read_only
        self.destructive = destructive
        self.idempotent = idempotent
        self.raw = raw

    def listing(self):
        annotations = {"title": self.title, "readOnlyHint": self.read_only, "openWorldHint": False}
        if not self.read_only:
            annotations["destructiveHint"] = self.destructive
            annotations["idempotentHint"] = self.idempotent
        return {"name": self.name, "title": self.title, "description": self.description,
                "inputSchema": self.schema, "annotations": annotations}


def tool(name, title, description, properties=None, required=(), *, read_only=False, destructive=False,
         idempotent=False, raw=False):
    """Registers a handler. Annotations follow the MCP hints; read_only mode hides every tool without readOnlyHint."""
    schema = {"type": "object", "properties": properties or {}}
    if required:
        schema["required"] = list(required)

    def register(handler):
        TOOLS[name] = Tool(name, title, description, schema, handler, read_only, destructive, idempotent, raw)
        return handler
    return register


class Blocks(list):
    """A result made of MCP content blocks (text and images) rather than one JSON object."""


def _text(value):
    return {"type": "text", "text": json.dumps(value, indent=2)}


def _render(result):
    if isinstance(result, Blocks):
        return {"content": list(result)}
    return {"content": [_text(result)], "structuredContent": result}


# Argument helpers ---------------------------------------------------------------

def _string(args, key, required=True):
    value = args.get(key)
    if value is None:
        if required:
            raise _ToolError(f"{key} is required")
        return None
    if not isinstance(value, str) or not value:
        raise _ToolError(f"{key} must be a non-empty string")
    return value


def _integer(args, key, default=None, minimum=None, maximum=None):
    value = args.get(key)
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, int):
        raise _ToolError(f"{key} must be a whole number")
    if (minimum is not None and value < minimum) or (maximum is not None and value > maximum):
        raise _ToolError(f"{key} must be between {minimum} and {maximum}")
    return value


def _number(args, key, required=False):
    value = args.get(key)
    if value is None:
        if required:
            raise _ToolError(f"{key} is required")
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise _ToolError(f"{key} must be a number")
    return value


def _boolean(args, key, default=False):
    value = args.get(key)
    if value is None:
        return default
    if not isinstance(value, bool):
        raise _ToolError(f"{key} must be true or false")
    return value


def _choice(args, key, choices, required=True):
    value = args.get(key)
    if value is None and not required:
        return None
    if value not in choices:
        raise _ToolError(f"{key} must be one of: {', '.join(choices)}")
    return value


def _pick(args, keys):
    return {key: args[key] for key in keys if args.get(key) is not None}


def _paths(args, limit):
    paths = args.get("paths")
    if not isinstance(paths, list) or not paths or not all(isinstance(path, str) and path for path in paths):
        raise _ToolError("paths must be a non-empty list of paths")
    if len(paths) > limit:
        raise _ToolError(f"at most {limit} paths per call")
    return paths


# Host access ---------------------------------------------------------------------

def host_call(method, params=None):
    """Runs a host method on the GTK main thread and waits for its result."""
    if not isinstance(method, str) or not method:
        raise _ToolError("method must be a non-empty string")
    done = threading.Event()
    outcome = {}

    def run():
        try:
            outcome["result"] = rsimgv.host.call(method, params or {})
        except Exception as exc:
            outcome["error"] = str(exc)
        done.set()
        return False

    rsimgv.ui.idle_add(run)
    if not done.wait(CALL_TIMEOUT):
        raise _ToolError(f"{method} timed out")
    if "error" in outcome:
        raise _ToolError(outcome["error"])
    return outcome["result"]


def _try(method, params=None):
    """The host method's result, or None when it fails (for example, view.info with no viewer open)."""
    try:
        return host_call(method, params)
    except _ToolError:
        return None


def run_action(name, value=None, target=None):
    if _action_excluded(name):
        raise _ToolError(f"{name} is not available over MCP")
    params = {"name": name}
    if value is not None:
        params["value"] = value
    if target is not None:
        params["target"] = target
    result = host_call("actions.run", params)
    return {"ran": name, **({"result": result} if result else {})}


def _current_path():
    return (_try("window.info") or {}).get("current_path")


# Schema fragments ----------------------------------------------------------------

_STR = {"type": "string"}
_INT = {"type": "integer"}


def _enum(values, description=None):
    schema = {"type": "string", "enum": list(values)}
    if description:
        schema["description"] = description
    return schema


_FORMAT = {"type": "string", "enum": ["png", "jpeg"], "default": "png",
           "description": "Image format. jpeg is much smaller than png."}
_QUALITY = {"type": "integer", "minimum": 1, "maximum": 100, "default": 85,
            "description": "Quality for jpeg; ignored for png."}
_MAX_SIZE = {"type": "integer", "minimum": 1, "maximum": 8192,
             "description": "Longest side in pixels. Larger pictures are shrunk, smaller ones are not enlarged."}
_CROP = {"type": "object", "description": "Region to keep, as fractions from 0 to 1 of the full picture, "
         "measured from its top left. Applied before max_size.",
         "properties": {"x": {"type": "number", "minimum": 0, "exclusiveMaximum": 1},
                        "y": {"type": "number", "minimum": 0, "exclusiveMaximum": 1},
                        "width": {"type": "number", "exclusiveMinimum": 0, "maximum": 1},
                        "height": {"type": "number", "exclusiveMinimum": 0, "maximum": 1}},
         "required": ["x", "y", "width", "height"]}
_LIMIT = {"type": "integer", "minimum": 1, "maximum": 500, "default": 100}
_OFFSET = {"type": "integer", "minimum": 0, "default": 0}


# Observe -------------------------------------------------------------------------

@tool("get_status", "Get status",
      "Orient yourself in one call: the folder shown, the selected file, the tab, and whether the viewer is open "
      "with what it shows.", read_only=True)
def get_status(args):
    window = host_call("window.info")
    tab = _try("tabs.current") or {}
    selected = _try("files.selected") or {}
    viewer = _try("view.info")
    return {"folder": window.get("current_path"), "selected_file": selected.get("name"),
            "selected_index": selected.get("index"), "tab": tab, "show_sidebar": window.get("show_sidebar"),
            "viewer_open": viewer is not None, "viewer": viewer}


@tool("view_info", "Describe the viewed item",
      "Describe the picture or video in the viewer: filename, size, position, duration, fps, speed. Fails when the "
      "viewer is not open.", read_only=True)
def view_info(args):
    return host_call("view.info")


@tool("view_screenshot", "Capture the viewer",
      "Capture the picture or video frame the viewer shows, as an image. Use max_size to keep it small and crop "
      "to look closely at one region. With info, the text also holds what view_info reports for the item.",
      {"format": _FORMAT, "quality": _QUALITY, "max_size": _MAX_SIZE, "crop": _CROP,
       "info": {"type": "boolean", "default": False,
                "description": "Also return view_info's result under the key info."}}, read_only=True)
def view_screenshot(args):
    want_info = _boolean(args, "info")
    result = host_call("view.screenshot", _pick(args, ("format", "quality", "max_size", "crop")))
    if not result.get("data"):
        raise _ToolError("the viewer has no picture to capture")
    details = {key: result.get(key) for key in ("format", "width", "height", "source_width", "source_height")}
    if want_info:
        details["info"] = _try("view.info")
    return Blocks([
        {"type": "image", "data": result["data"], "mimeType": result.get("mime_type", "image/png")},
        _text(details),
    ])


@tool("get_thumbnails", "Get thumbnails",
      "Get the thumbnail images rsimgv has already made for files. A file without one is reported as missing; "
      "open its folder in rsimgv so thumbnails are made. The size is the largest cached thumbnail not above "
      "max_size.",
      {"paths": {"type": "array", "items": _STR, "minItems": 1, "maxItems": MAX_THUMBNAILS},
       "max_size": {"type": "integer", "minimum": 1, "maximum": 2048, "default": 256},
       "format": _FORMAT, "quality": _QUALITY}, ["paths"], read_only=True)
def get_thumbnails(args):
    params = _pick(args, ("max_size", "format", "quality"))
    params["paths"] = _paths(args, MAX_THUMBNAILS)
    blocks, found, missing = [], [], []
    for item in host_call("thumbnails.get", params).get("thumbnails", []):
        if item.get("data"):
            blocks.append({"type": "image", "data": item["data"], "mimeType": item.get("mime_type", "image/png")})
            found.append({key: item.get(key) for key in ("path", "size", "width", "height", "format")})
        else:
            missing.append(item.get("path"))
    # The text lists the images in the order they follow it.
    return Blocks([_text({"images": found, "missing": missing})] + blocks)


@tool("get_file_info", "Get file information",
      "Size, type and media details (dimensions, duration, codec) for files. media_pending means the file has "
      "been queued: ask again shortly.",
      {"paths": {"type": "array", "items": _STR, "minItems": 1, "maxItems": MAX_METADATA_PATHS}},
      ["paths"], read_only=True)
def get_file_info(args):
    result = host_call("metadata.get", {"paths": _paths(args, MAX_METADATA_PATHS)})
    return {"files": result.get("metadata", [])}


@tool("get_file_state", "Get saved file state",
      "The state rsimgv saved for a file, such as where a video was left.", {"path": _STR}, ["path"],
      read_only=True)
def get_file_state(args):
    return host_call("files.state", {"path": _string(args, "path")})


# Browse and select ----------------------------------------------------------------

def _is_media(name):
    kind = (mimetypes.guess_type(name)[0] or "")
    return kind.startswith(("image/", "video/"))


@tool("list_files", "List files",
      "List the items the current tab shows, in its order, or a folder's contents from disk when path is given. "
      "media_only keeps pictures and videos (by file extension) and leaves out folders.",
      {"path": _STR, "query": {"type": "string", "description": "Keep names containing this text, ignoring case."},
       "media_only": {"type": "boolean", "default": False}, "limit": _LIMIT, "offset": _OFFSET}, read_only=True)
def list_files(args):
    path = _string(args, "path", required=False)
    query = (_string(args, "query", required=False) or "").lower()
    media_only = _boolean(args, "media_only")
    limit = _integer(args, "limit", 100, 1, 500)
    offset = _integer(args, "offset", 0, 0)
    listed = host_call("files.list", {"path": path} if path else {}).get("files", {})
    files = [{"name": name, **info} for name, info in listed.items()
             if query in name.lower() and (not media_only or (not info.get("is_dir") and _is_media(name)))]
    return {"path": path or _current_path(), "total": len(files), "offset": offset,
            "files": files[offset:offset + limit]}


@tool("open_folder", "Open a folder", "Browse a folder in the current tab.", {"path": _STR}, ["path"],
      idempotent=True)
def open_folder(args):
    host_call("navigate.to", {"path": _string(args, "path")})
    return {"current_path": _current_path()}


@tool("navigate", "Navigate history",
      "Go back or forward in the folder history, or up to the parent folder, n times (default 1).",
      {"direction": _enum(["back", "forward", "up"]), "n": {"type": "integer", "minimum": 1, "default": 1}},
      ["direction"])
def navigate(args):
    direction = _choice(args, "direction", ("back", "forward", "up"))
    moved = host_call(f"navigate.{direction}", {"n": _integer(args, "n", 1, 1, 100)}).get("moved")
    return {"moved": moved, "current_path": _current_path()}


@tool("step_folder", "Step through folders",
      "Go to the next or previous folder in tree order. With media_only, skip folders without pictures or "
      "videos. path is null when there is no further folder.",
      {"direction": _enum(["next", "previous"]), "media_only": {"type": "boolean", "default": False}},
      ["direction"])
def step_folder(args):
    direction = _choice(args, "direction", ("next", "previous"))
    method = f"navigate.{direction}_media" if _boolean(args, "media_only") else f"navigate.{direction}"
    return host_call(method)


@tool("select_file", "Select a file",
      "Select an item in the current tab by name or path (file) or by position (index; negative counts from "
      "the end). Give exactly one. This selects; it does not open the item.",
      {"file": _STR, "index": _INT}, idempotent=True)
def select_file(args):
    file, index = _string(args, "file", required=False), _integer(args, "index")
    if (file is None) == (index is None):
        raise _ToolError("give exactly one of file and index")
    return host_call("files.select", {"file": file}) if file else host_call("files.select_index", {"index": index})


@tool("open_item", "Open the selected item",
      "Show the selected picture or video in the viewer. Does nothing if the viewer is already open.",
      idempotent=True)
def open_item(args):
    if _try("view.info") is not None:
        return {"viewer_open": True, "changed": False}
    run_action("win.toggle-viewer")
    return {"viewer_open": True, "changed": True}


@tool("close_viewer", "Close the viewer", "Close the viewer and return to the browse view.", idempotent=True)
def close_viewer(args):
    if _try("view.info") is None:
        return {"viewer_open": False, "changed": False}
    run_action("view.close")
    return {"viewer_open": False, "changed": True}


# Tabs and bookmarks -----------------------------------------------------------------

@tool("list_tabs", "List tabs", "List the open tabs.", read_only=True)
def list_tabs(args):
    return host_call("tabs.list")


@tool("open_tab", "Open a tab", "Open a folder in a new tab.", {"path": _STR, "activate": {"type": "boolean"}},
      ["path"])
def open_tab(args):
    return host_call("tabs.open", {"path": _string(args, "path"), "activate": _boolean(args, "activate")})


@tool("activate_tab", "Switch tab", "Switch to a tab by index (from 0) or id. Give one.",
      {"index": _INT, "id": _INT}, idempotent=True)
def activate_tab(args):
    index, ident = _integer(args, "index"), _integer(args, "id")
    if (index is None) == (ident is None):
        raise _ToolError("give exactly one of index and id")
    return host_call("tabs.activate", {"index": index} if index is not None else {"id": ident})


@tool("close_tab", "Close a tab", "Close a tab by index.", {"index": _INT}, ["index"], destructive=True)
def close_tab(args):
    if "index" not in args:
        raise _ToolError("index is required")
    return host_call("tabs.close", {"index": _integer(args, "index")})


@tool("list_bookmarks", "List bookmarks", "List bookmarked folders.", read_only=True)
def list_bookmarks(args):
    return host_call("bookmarks.list")


@tool("add_bookmark", "Add a bookmark", "Bookmark a folder. The name defaults to the folder's own.",
      {"path": _STR, "name": _STR}, ["path"], idempotent=True)
def add_bookmark(args):
    return host_call("bookmarks.add", _pick({"path": _string(args, "path"), "name": _string(args, "name", False)},
                                            ("path", "name")))


@tool("remove_bookmark", "Remove a bookmark", "Remove a folder's bookmark.", {"path": _STR}, ["path"],
      destructive=True, idempotent=True)
def remove_bookmark(args):
    return host_call("bookmarks.remove", {"path": _string(args, "path")})


# Viewer and playback --------------------------------------------------------------------

VIEWER_NAVIGATE = ("previous", "next", "first", "last")
IMAGE_COMMANDS = {
    "zoom_in": "image.zoom-in", "zoom_out": "image.zoom-out", "fit_window": "image.fit-window",
    "fit_width": "image.fit-width", "fit_height": "image.fit-height", "fill": "image.fit-fill",
    "original": "image.fit-original", "cycle_fit": "image.fit-cycle", "pan_up": "image.pan-up",
    "pan_down": "image.pan-down", "pan_left": "image.pan-left", "pan_right": "image.pan-right",
    "toggle_upscaled": "image.toggle-upscaled", "compare_upscaled": "image.compare-upscaled",
}
TRANSFORM_COMMANDS = {
    "rotate_cw": "view.rotate-clockwise", "rotate_ccw": "view.rotate-counter-clockwise",
    "flip_h": "view.flip-horizontal", "flip_v": "view.flip-vertical", "set_rotation": "view.set-rotation",
}
VIDEO_PLAYBACK_COMMANDS = {
    "play_pause": "video.play-pause", "seek": "video.seek", "frame_step": "video.frame-seek",
    "set_speed": "video.set-speed", "repeat": "video.repeat", "mute": "video.mute", "volume": "video.volume",
}
VIDEO_VIEW_COMMANDS = {
    "zoom_in": "video.zoom-in", "zoom_out": "video.zoom-out", "zoom_reset": "video.zoom-reset",
    "pan_up": "video.pan-up", "pan_down": "video.pan-down", "pan_left": "video.pan-left",
    "pan_right": "video.pan-right", "toggle_crop": "video.toggle-crop", "toggle_backdrop": "video.toggle-backdrop",
    "toggle_svp": "video.toggle-svp", "set_aspect": "video.set-aspect", "cycle_aspect": "video.cycle-aspect",
}
ASPECTS = ("auto", "4:3", "16:9", "9:16", "1:1", "3:2", "2:3", "21:9")


@tool("viewer_navigate", "Move through the viewer",
      "Show the previous, next, first or last item of the folder (or of the playlist while the viewer shows "
      "it). Use get_status afterwards to see what is shown.", {"to": _enum(VIEWER_NAVIGATE)}, ["to"])
def viewer_navigate(args):
    return run_action(f"view.{_choice(args, 'to', VIEWER_NAVIGATE)}")


@tool("image_view", "Zoom and fit a picture",
      "Zoom, fit mode and panning of a picture. Zoom and pan commands repeat `steps` times. The fit commands set "
      "a mode; cycle_fit, toggle_upscaled and compare_upscaled switch state.",
      {"command": _enum(IMAGE_COMMANDS), "steps": {"type": "integer", "minimum": 1, "maximum": MAX_STEPS,
                                                    "default": 1}}, ["command"])
def image_view(args):
    action = IMAGE_COMMANDS[_choice(args, "command", IMAGE_COMMANDS)]
    steps = _integer(args, "steps", 1, 1, MAX_STEPS)
    for _ in range(steps):
        result = run_action(action)
    return {**result, "steps": steps}


@tool("transform_view", "Rotate or flip",
      "Turn or mirror the picture or video. set_rotation needs degrees (0, 90, 180 or 270, clockwise from how "
      "the file has it).",
      {"command": _enum(TRANSFORM_COMMANDS), "degrees": {"type": "integer", "enum": [0, 90, 180, 270]}},
      ["command"])
def transform_view(args):
    command = _choice(args, "command", TRANSFORM_COMMANDS)
    if command != "set_rotation":
        return run_action(TRANSFORM_COMMANDS[command])
    degrees = _integer(args, "degrees")
    if degrees not in (0, 90, 180, 270):
        raise _ToolError("degrees must be 0, 90, 180 or 270")
    return run_action(TRANSFORM_COMMANDS[command], degrees)


@tool("video_playback", "Control video playback",
      "Play or pause, seek, step frames, change speed, repeat, mute or change the volume. value is seconds for "
      "seek (negative goes back), frames for frame_step, a multiple of normal speed for set_speed (0.05 to 16, "
      "1 is normal) and a percentage change for volume (negative lowers). target says whether volume changes "
      "the current video or the application.",
      {"command": _enum(VIDEO_PLAYBACK_COMMANDS), "value": {"type": "number"},
       "target": _enum(["video", "application"])}, ["command"])
def video_playback(args):
    command = _choice(args, "command", VIDEO_PLAYBACK_COMMANDS)
    value = _number(args, "value", required=command in ("seek", "frame_step", "set_speed", "volume"))
    target = _choice(args, "target", ("video", "application"), required=False)
    if target and command != "volume":
        raise _ToolError("target applies to volume only")
    return run_action(VIDEO_PLAYBACK_COMMANDS[command], value, target)


@tool("video_view", "Frame a video",
      "Zoom and pan a video, change its aspect ratio (set_aspect needs aspect; auto is the video's own), or "
      "toggle crop detection, the blurred backdrop or SVP interpolation.",
      {"command": _enum(VIDEO_VIEW_COMMANDS), "aspect": _enum(ASPECTS)}, ["command"])
def video_view(args):
    command = _choice(args, "command", VIDEO_VIEW_COMMANDS)
    target = _choice(args, "aspect", ASPECTS) if command == "set_aspect" else None
    return run_action(VIDEO_VIEW_COMMANDS[command], target=target)


@tool("slideshow", "Slideshow", "Start or stop the slideshow (toggle), or switch its shuffle order (toggle_shuffle). "
      "There is no way to read whether it is running.", {"command": _enum(["toggle", "toggle_shuffle"])},
      ["command"])
def slideshow(args):
    command = _choice(args, "command", ("toggle", "toggle_shuffle"))
    return run_action("view.toggle-slideshow" if command == "toggle" else "view.toggle-slideshow-shuffle")


PLAYLIST_COMMANDS = ("play", "next", "previous", "first", "last", "shuffle")


@tool("playlist_control", "Control the playlist",
      "Show the playlist in the viewer (play), move through it, or switch its shuffle order (shuffle).",
      {"command": _enum(PLAYLIST_COMMANDS)}, ["command"])
def playlist_control(args):
    return run_action(f"win.playlist-{_choice(args, 'command', PLAYLIST_COMMANDS)}")


@tool("playlist_toggle_item", "Add or remove from the playlist",
      "Add the item the viewer shows (or, while browsing, the marked items or the one under the cursor) to the "
      "playlist, or remove it if it is there.")
def playlist_toggle_item(args):
    return run_action("win.playlist-toggle")


# Settings and messages ----------------------------------------------------------------------

@tool("describe_settings", "Describe settings",
      "List application settings with type, description, default and current value, optionally filtered by text "
      "in the key or description.",
      {"query": _STR, "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}}, read_only=True)
def describe_settings(args):
    query = (_string(args, "query", required=False) or "").lower()
    limit = _integer(args, "limit", 50, 1, 200)
    settings = [setting for setting in host_call("settings.describe").get("settings", [])
                if not _setting_excluded(setting.get("key", ""))
                and (not query or query in setting.get("key", "").lower()
                     or query in (setting.get("description") or "").lower())]
    return {"total": len(settings), "settings": settings[:limit]}


@tool("get_setting", "Read a setting", "Read one application setting.", {"key": _STR}, ["key"], read_only=True)
def get_setting(args):
    return host_call("settings.get", {"key": _string(args, "key")})


@tool("set_setting", "Change a setting",
      "Change an application setting; a null value resets it to its default. Settings that decide which plugins "
      "load are not available.", {"key": _STR, "value": {}}, ["key"], idempotent=True)
def set_setting(args):
    key = _string(args, "key")
    if _setting_excluded(key):
        raise _ToolError(f"{key} is not available over MCP")
    return host_call("settings.set", {"key": key, "value": args.get("value")})


@tool("show_message", "Show a message", "Show a message over the rsimgv window, for the person at the desktop.",
      {"text": _STR, "level": _enum(["info", "success", "warning", "error", "plain"]),
       "seconds": {"type": "number", "exclusiveMinimum": 0, "default": 4},
       "position": _enum(["top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right"])},
      ["text"])
def show_message(args):
    params = {"text": _string(args, "text")}
    params.update(_pick(args, ("level", "position")))
    seconds = _number(args, "seconds")
    if seconds is not None:
        if seconds <= 0:
            raise _ToolError("seconds must be above 0")
        params["seconds"] = seconds
    return host_call("messages.show", params)


# Discovery and the escape hatches ----------------------------------------------------------

SCOPES = ("global", "browse", "view", "image", "video", "playlist", "tree")


@tool("list_actions", "Find actions",
      "Find actions for run_action by text in the name, label or description, optionally limited to a scope. "
      "Includes plugin actions. Most things have a dedicated tool; use this for the rest.",
      {"query": _STR, "scope": _enum(SCOPES), "limit": {"type": "integer", "minimum": 1, "maximum": 200,
                                                          "default": 50}}, read_only=True)
def list_actions(args):
    query = (_string(args, "query", required=False) or "").lower()
    scope = _choice(args, "scope", SCOPES, required=False)
    limit = _integer(args, "limit", 50, 1, 200)
    actions = []
    for action in host_call("actions.list").get("actions", []):
        name = action.get("name", "")
        if _action_excluded(name):
            continue
        if scope and scope != action.get("scope") and scope not in (action.get("contexts") or []):
            continue
        if query and not any(query in (action.get(key) or "").lower() for key in ("name", "label", "description")):
            continue
        actions.append({key: action.get(key) for key in ("name", "label", "description", "scope")})
    return {"total": len(actions), "actions": actions[:limit]}


@tool("run_action", "Run an action",
      "Run an action by name (find it with list_actions). value is a number and target a string, for the few "
      "actions that take them. Plugin actions can take up to two minutes.",
      {"name": _STR, "value": {"type": "number"}, "target": _STR}, ["name"])
def run_action_tool(args):
    return run_action(_string(args, "name"), _number(args, "value"), _string(args, "target", required=False))


@tool("list_plugins", "List plugins", "List the plugins rsimgv has loaded.", read_only=True)
def list_plugins(args):
    result = host_call("plugins.list")
    return result if isinstance(result, dict) else {"plugins": result}


@tool("call_method", "Call a host method",
      "Call any host method by name, as on the IPC socket (see src/services/remote.rs). It bypasses the limits "
      "the other tools apply. Off unless the plugin's allow_raw_methods setting is on.",
      {"method": _STR, "params": {"type": "object"}}, ["method"], destructive=True, raw=True)
def call_method(args):
    params = args.get("params") or {}
    if not isinstance(params, dict):
        raise _ToolError("params must be an object")
    return host_call(_string(args, "method"), params)


class Plugin:
    def __init__(self):
        self._server = None
        self._thread = None
        self._token = ""
        self._read_only = False
        self._allow_raw = False

    def activate(self, host):
        config = self._load_config()
        self._token = str(config["token"])
        self._read_only = bool(config["read_only"])
        self._allow_raw = bool(config["allow_raw_methods"])
        bind = str(config["bind_address"]).strip() or DEFAULTS["bind_address"]
        if not self._is_loopback(bind) and not self._token:
            rsimgv.notify("MCP Server: set an access token before listening on "
                          f"{bind}; the server is not running.", level="error", seconds=10)
            return
        plugin = self
        port = int(config["port"])

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, fmt, *args):
                pass

            def do_POST(self):
                plugin._post(self)

            def do_GET(self):
                self._send(405, b"", {"Allow": "POST"})

            def do_DELETE(self):
                self._send(405, b"", {"Allow": "POST"})

            def _send(self, status, body=b"", headers=None):
                self.send_response(status)
                for key, value in (headers or {}).items():
                    self.send_header(key, value)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                if body:
                    self.wfile.write(body)

        try:
            self._server = ThreadingHTTPServer((bind, port), Handler)
        except OSError as exc:
            rsimgv.notify(f"MCP Server: cannot listen on {bind}:{port}: {exc}", level="error", seconds=10)
            return
        self._server.daemon_threads = True
        self._thread = threading.Thread(target=self._server.serve_forever, name="rsimgv-mcp", daemon=True)
        self._thread.start()
        rsimgv.host.log("info", f"MCP server listening on http://{bind}:{port}/mcp")

    def deactivate(self):
        server, self._server = self._server, None
        if server is not None:
            threading.Thread(target=server.shutdown, daemon=True).start()
            server.server_close()

    # HTTP -----------------------------------------------------------------

    def _post(self, handler):
        if not self._allowed(handler):
            handler._send(403, b"forbidden")
            return
        if urlsplit(handler.path).path != "/mcp":
            handler._send(404, b"not found")
            return
        try:
            length = int(handler.headers.get("Content-Length", "0"))
        except ValueError:
            length = -1
        if not 0 < length <= MAX_BODY:
            handler._send(400, b"bad request")
            return
        try:
            message = json.loads(handler.rfile.read(length))
        except ValueError:
            self._reply(handler, {"jsonrpc": "2.0", "id": None,
                                  "error": {"code": -32700, "message": "parse error"}})
            return
        if isinstance(message, list):
            responses = [r for r in (self._dispatch(m) for m in message) if r is not None]
            if responses:
                self._reply(handler, responses)
            else:
                handler._send(202)
            return
        response = self._dispatch(message)
        if response is None:
            handler._send(202)
        else:
            self._reply(handler, response)

    def _reply(self, handler, payload):
        body = json.dumps(payload).encode("utf-8")
        handler._send(200, body, {"Content-Type": "application/json"})

    def _allowed(self, handler):
        if self._token:
            # The token authenticates the client, so Host and Origin need no check.
            supplied = handler.headers.get("Authorization") or ""
            return hmac.compare_digest(supplied.encode(), f"Bearer {self._token}".encode())
        # Without a token the server is loopback-only; reject DNS rebinding.
        host = urlsplit("//" + (handler.headers.get("Host") or "")).hostname
        if host not in LOCAL_HOSTS:
            return False
        origin = handler.headers.get("Origin")
        if origin and (urlsplit(origin).hostname or "") not in ("127.0.0.1", "localhost", "::1"):
            return False
        return True

    @staticmethod
    def _is_loopback(address):
        if address == "localhost":
            return True
        try:
            return ipaddress.ip_address(address).is_loopback
        except ValueError:
            return False

    # JSON-RPC -------------------------------------------------------------

    def _enabled(self, tool):
        if self._read_only and not tool.read_only:
            return False
        return self._allow_raw or not tool.raw

    def _dispatch(self, message):
        if not isinstance(message, dict):
            return {"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "invalid request"}}
        method = message.get("method")
        ident = message.get("id")
        params = message.get("params") or {}
        if ident is None:  # a notification or stray response
            return None
        try:
            if method == "initialize":
                requested = params.get("protocolVersion")
                version = requested if requested in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0]
                result = {
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "rsimgv", "version": "0.1.0"},
                    "instructions": "Controls the rsimgv media viewer and file explorer. Start with get_status. "
                                    "Use list_actions and run_action for anything without its own tool.",
                }
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": [tool.listing() for tool in TOOLS.values() if self._enabled(tool)]}
            elif method == "tools/call":
                result = self._call_tool(params.get("name"), params.get("arguments") or {})
            else:
                return {"jsonrpc": "2.0", "id": ident,
                        "error": {"code": -32601, "message": f"unknown method {method}"}}
        except Exception as exc:  # a failure outside the tool itself
            return {"jsonrpc": "2.0", "id": ident, "error": {"code": -32603, "message": str(exc)}}
        return {"jsonrpc": "2.0", "id": ident, "result": result}

    def _call_tool(self, name, arguments):
        tool = TOOLS.get(name)
        if tool is None or not self._enabled(tool):
            raise ValueError(f"unknown tool {name}")
        if not isinstance(arguments, dict):
            raise ValueError("arguments must be an object")
        try:
            return _render(tool.handler(arguments))
        except _ToolError as exc:
            return {"content": [{"type": "text", "text": str(exc)}], "isError": True}

    def _load_config(self):
        config = dict(DEFAULTS)
        path = os.path.join(os.path.dirname(rsimgv.host.config_path()), "config.json")
        try:
            with open(path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
            if isinstance(loaded, dict):
                config.update({k: v for k, v in loaded.items() if k in DEFAULTS})
        except FileNotFoundError:
            pass
        except Exception as exc:
            rsimgv.notify(f"MCP Server: cannot read {path}: {exc}", level="warning", seconds=8)
        try:
            config["port"] = int(config["port"])
        except (TypeError, ValueError):
            config["port"] = DEFAULTS["port"]
        return config
