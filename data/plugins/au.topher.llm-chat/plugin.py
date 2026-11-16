"""LLM Chat: a persistent chat in the sidebar about the picture being viewed.

Every message is sent with the viewer's current picture or video frame, its
filename and its metadata. The conversation is kept in a file beside the
plugin's config and restored when rsimgv starts. Replies are shown as markdown,
with the model's reasoning in a block that collapses when the answer starts.
Voice input records with an external program and transcribes with an HTTP
endpoint or a local command; spoken replies are synthesized the same way and
played by an external program, with the markdown left out.

Widget signals of a panel are connected in ``create_panel``, the one place the
host gives the plugin a context that ``rsimgv.ui.connect`` accepts. Widgets made
later, such as message bubbles, use plain ``connect``: they are destroyed with
their panel. Network requests and audio programs run on worker threads, and
their results are posted back with ``rsimgv.ui.idle_add``, which runs on the
GTK main thread.
"""

import array
import base64
import collections
import copy
import difflib
import json
import math
import os
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
import wave
import xml.etree.ElementTree as ElementTree

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
gi.require_version("GdkPixbuf", "2.0")
gi.require_version("Pango", "1.0")
gi.require_version("Graphene", "1.0")
from gi.repository import Gdk, GdkPixbuf, GLib, Graphene, Gtk, Pango

import rsimgv


NAME = "LLM Chat"

# Set RSIMGV_CHAT_DEBUG=1 to log the chat's scrolling to the terminal rsimgv was started from.
DEBUG = bool(os.environ.get("RSIMGV_CHAT_DEBUG"))

# The API rejects very large requests, and a base64 PNG grows quickly.
MAX_BASE64 = 20_000_000
# The picture sent to the model is shrunk until neither side is longer than this, never enlarged, and sent as a JPEG
# of this quality.
SENT_IMAGE_SIZE = 768
SENT_IMAGE_QUALITY = 80
# The most messages kept on disk. Older ones are dropped.
MAX_STORED = 500
# OpenAI's speech endpoint takes at most 4096 characters.
MAX_SPOKEN = 4000
# How near the bottom, in pixels, the log counts as following new messages.
FOLLOW_SLACK = 24

# The viewer card's size when it has not been resized, and the smallest it can be made.
CARD_SIZE = (380, 480)
CARD_MIN = (280, 220)

# The corners of the viewer card that resize it, with where each sits and the pointer shown over it.
CORNERS = {
    "top-left": (Gtk.Align.START, Gtk.Align.START, "nw-resize"),
    "top-right": (Gtk.Align.END, Gtk.Align.START, "ne-resize"),
    "bottom-left": (Gtk.Align.START, Gtk.Align.END, "sw-resize"),
    "bottom-right": (Gtk.Align.END, Gtk.Align.END, "se-resize"),
}
CORNER_SIZE = 12

# The longest side of the picture kept with each message you send, and shown in its bubble.
THUMB_SIZE = 256

DEFAULTS = {
    "base_url": "https://api.openai.com/v1",
    "model": "gpt-4o-mini",
    "api_key_env": "OPENAI_API_KEY",
    "system_prompt": (
        "You are a helpful assistant inside an image and video viewer. Each "
        "message comes with the item the user is looking at, its filename and "
        "its metadata. Answer about what is shown."
    ),
    "send_image": True,
    "send_metadata": True,
    "stream": True,
    "history_messages": 20,
    "max_tokens": 1000,
    "timeout_seconds": 120,
    "asr_backend": "off",
    "asr_url": "",
    "asr_model": "whisper-1",
    "asr_command": "whisper-cli --no-timestamps --no-prints -f {input}",
    "record_command": "parecord --rate=16000 --channels=1 --file-format=wav {output}",
    "auto_send_voice": True,
    "tts_backend": "off",
    "tts_url": "",
    "tts_model": "tts-1",
    "tts_voice": "alloy",
    "tts_command": "",
    "play_command": "ffplay -nodisp -autoexit -loglevel quiet {input}",
    "auto_speak": False,
    "voice_prompt": (
        "This is a spoken conversation. The user's messages are transcribed from "
        "speech, so they may contain mistakes; if one makes no sense, ask briefly "
        "what they meant. Your reply will be read aloud by a text-to-speech voice, "
        "so answer the way a person talks: short, natural sentences, usually one "
        "to three. Use plain text only, with no markdown, lists, tables, code, "
        "emoji or web addresses, and spell out anything that would be awkward to "
        "hear. Refer to the picture naturally, as if you can both see it."
    ),
    "vad_record_command": "parecord --raw --format=s16le --rate=16000 --channels=1 --latency-msec=30",
    "vad_threshold": 400,
    "vad_silence_ms": 1000,
    "card_opacity": 100,
    "temperature": "",
    "top_p": "",
    "extra_params": "",
    "thinking": True,
    "thinking_on_params": "",
    "thinking_off_params": '{"chat_template_kwargs": {"enable_thinking": false}}',
    "memory": True,
    "memory_limit": 100,
    "barge_in": True,
    "chat_sidebar": True,
    "user_name": "",
    "tools": True,
    "quick_commands": True,
    "tool_viewer": True,
    "tool_rounds": 5,
}

LIMITS = {
    "history_messages": (1, 200),
    "max_tokens": (1, 32768),
    "timeout_seconds": (5, 600),
    "vad_threshold": (50, 8000),
    "vad_silence_ms": (200, 5000),
    "card_opacity": (20, 100),
    "memory_limit": (1, 1000),
    "tool_rounds": (1, 10),
}

BACKENDS = ("off", "http", "command")


# -- markdown ------------------------------------------------------------

CODE_STYLE = 'font_family="monospace" background="#808080" bgalpha="20%"'
HEADING_SIZES = {1: "xx-large", 2: "x-large", 3: "large"}


def deep_merge(target, source):
    """Merges `source` into `target`, going into objects that both have rather than replacing them."""
    for key, value in source.items():
        if isinstance(value, dict) and isinstance(target.get(key), dict):
            deep_merge(target[key], value)
        else:
            target[key] = copy.deepcopy(value)


def drop_alpha(pixbuf):
    """The picture with no alpha channel, its transparent parts filled with white.

    The JPEG saver loses the colour of a picture that has an alpha channel, even a fully opaque one, so the
    channel has to be taken out and not only flattened.
    """
    width, height = pixbuf.get_width(), pixbuf.get_height()
    flat = pixbuf.composite_color_simple(
        width, height, GdkPixbuf.InterpType.BILINEAR, 255, 16, 0xFFFFFFFF, 0xFFFFFFFF
    )
    stride, pixels = flat.get_rowstride(), flat.get_pixels()
    rgb = bytearray(width * height * 3)
    for y in range(height):
        row = pixels[y * stride : y * stride + width * 4]
        base = y * width * 3
        end = base + width * 3
        rgb[base:end:3] = row[0::4]
        rgb[base + 1 : end : 3] = row[1::4]
        rgb[base + 2 : end : 3] = row[2::4]
    return GdkPixbuf.Pixbuf.new_from_bytes(
        GLib.Bytes.new(bytes(rgb)), GdkPixbuf.Colorspace.RGB, False, 8, width, height, width * 3
    )


def scaled_jpeg(image_base64, longest_side, quality):
    """A base64 picture of any format as a base64 JPEG no longer than `longest_side` either way, or None if unreadable.

    A picture that is smaller already is not enlarged. Transparent parts are filled with white, which a JPEG
    cannot hold.
    """
    loader = GdkPixbuf.PixbufLoader()

    def fit(loader, width, height):
        scale = min(1.0, longest_side / max(width, height))
        loader.set_size(max(1, round(width * scale)), max(1, round(height * scale)))

    loader.connect("size-prepared", fit)
    failed = False
    try:
        loader.write(base64.b64decode(image_base64))
    except Exception:
        failed = True
    # The loader must always be closed, even after it failed.
    try:
        loader.close()
    except GLib.Error:
        failed = True
    pixbuf = None if failed else loader.get_pixbuf()
    if pixbuf is None:
        return None
    if pixbuf.get_has_alpha():
        pixbuf = drop_alpha(pixbuf)
    ok, data = pixbuf.save_to_bufferv("jpeg", ["quality"], [str(quality)])
    return base64.b64encode(data).decode("ascii") if ok else None


def make_thumbnail(image_base64):
    """The small JPEG kept with a message you sent, as base64, or None when the picture cannot be read."""
    return scaled_jpeg(image_base64, THUMB_SIZE, 80)


def thumbnail_texture(data):
    try:
        return Gdk.Texture.new_from_bytes(GLib.Bytes.new(base64.b64decode(data)))
    except Exception:
        return None


def escape(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def inline_markup(text):
    """One line of markdown as Pango markup. Anything not recognised stays as text."""
    text = escape(text)

    # Code spans are lifted out first so nothing inside them is formatted.
    spans = []

    def keep(match):
        spans.append(match.group(1))
        return f"\x00{len(spans) - 1}\x00"

    text = re.sub(r"`([^`\n]+)`", keep, text)
    text = re.sub(r"!\[([^\]]*)\]\([^)\s]*\)", r"\1", text)
    text = re.sub(
        r"\[([^\]]+)\]\((https?://[^)\s]+)\)",
        lambda m: f'<a href="{m.group(2).replace(chr(34), "%22")}">{m.group(1)}</a>',
        text,
    )
    text = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"<b>\1</b>", text)
    text = re.sub(r"(?<!\w)__(?=\S)(.+?)(?<=\S)__(?!\w)", r"<b>\1</b>", text)
    text = re.sub(r"(?<![\w*])\*(?=[^\s*])(.+?)(?<=[^\s*])\*(?![\w*])", r"<i>\1</i>", text)
    text = re.sub(r"(?<![\w_])_(?=[^\s_])(.+?)(?<=[^\s_])_(?![\w_])", r"<i>\1</i>", text)
    text = re.sub(r"~~(?=\S)(.+?)(?<=\S)~~", r"<s>\1</s>", text)
    return re.sub(
        r"\x00(\d+)\x00",
        lambda m: f"<span {CODE_STYLE}>{spans[int(m.group(1))]}</span>",
        text,
    )


def well_formed(markup):
    try:
        ElementTree.fromstring(f"<r>{markup}</r>")
    except ElementTree.ParseError:
        return False
    return True


def markdown_markup(text):
    """Markdown as Pango markup for a Gtk.Label. The result is always well formed."""
    rendered = []
    in_code = False
    for line in text.split("\n"):
        lines = []
        if line.strip().startswith("```"):
            in_code = not in_code
            continue
        if in_code:
            lines.append(f"<span {CODE_STYLE}>{escape(line) or ' '}</span>")
            rendered.append((lines[0], line))
            continue

        heading = re.match(r"^\s{0,3}(#{1,6})\s+(.*)$", line)
        bullet = re.match(r"^(\s*)[-*+]\s+(.*)$", line)
        numbered = re.match(r"^(\s*)(\d+[.)])\s+(.*)$", line)
        quote = re.match(r"^\s*>\s?(.*)$", line)
        if heading:
            size = HEADING_SIZES.get(len(heading.group(1)))
            attributes = f'size="{size}" ' if size else ""
            lines.append(f'<span {attributes}weight="bold">{inline_markup(heading.group(2))}</span>')
        elif re.fullmatch(r"\s*([-*_])(\s*\1){2,}\s*", line):
            lines.append("────────────")
        elif bullet:
            lines.append(f"{'    ' * (len(bullet.group(1)) // 2)}• {inline_markup(bullet.group(2))}")
        elif numbered:
            lines.append(
                f"{'    ' * (len(numbered.group(1)) // 2)}{numbered.group(2)} {inline_markup(numbered.group(3))}"
            )
        elif quote:
            lines.append(f"<i>▎ {inline_markup(quote.group(1))}</i>")
        else:
            lines.append(inline_markup(line))
        rendered.append((lines[0], line))

    # Emphasis can overlap a link or another span and break the nesting. A line
    # that does is shown as plain text rather than losing the whole message.
    return "\n".join(markup if well_formed(markup) else escape(raw) for markup, raw in rendered)


def spoken_text(text):
    """Markdown reduced to what reads well aloud."""
    text = re.sub(r"```.*?(```|\Z)", " ", text, flags=re.S)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"https?://\S+", "", text)
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"^\s{0,3}#{1,6}\s*", "", text, flags=re.M)
    text = re.sub(r"^\s*>\s?", "", text, flags=re.M)
    text = re.sub(r"^\s*[-*+]\s+", "", text, flags=re.M)
    text = re.sub(r"^[ \t]*\|?[ \t:|-]+\|?[ \t]*$", "", text, flags=re.M)
    text = text.replace("|", " ")
    text = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"\1", text)
    text = re.sub(r"(?<!\w)__(?=\S)(.+?)(?<=\S)__(?!\w)", r"\1", text)
    text = re.sub(r"(?<![\w*])\*(?=[^\s*])(.+?)(?<=[^\s*])\*(?![\w*])", r"\1", text)
    text = re.sub(r"(?<![\w_])_(?=[^\s_])(.+?)(?<=[^\s_])_(?![\w_])", r"\1", text)
    text = re.sub(r"~~(?=\S)(.+?)(?<=\S)~~", r"\1", text)
    return re.sub(r"\n{3,}", "\n\n", text).strip()


# -- memory --------------------------------------------------------------

MEMORY_TAGS = ("<memory>", "<forget>", "<tool_call>")


def strip_memory_tags(text):
    """A reply without the tags the model uses to save and forget memories, including one still arriving."""
    text = re.sub(r"<(memory|forget|tool_call)>.*?</\1>", "", text, flags=re.S)
    text = re.sub(r"<(?:memory|forget|tool_call)>.*\Z", "", text, flags=re.S)
    index = text.rfind("<")
    if index >= 0 and any(tag.startswith(text[index:]) for tag in MEMORY_TAGS):
        text = text[:index]
    return re.sub(r"\n{3,}", "\n\n", text).strip()


# -- tools ---------------------------------------------------------------


def tool(name, description, properties=None, required=None):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {"type": "object", "properties": properties or {}, "required": required or []},
        },
    }


def steps_property(what):
    return {"type": "integer", "minimum": 1, "maximum": 10, "description": f"How many {what}. One by default."}


MEMORY_TOOLS = [
    tool(
        "remember",
        "Save a fact in your persistent memory, which every conversation shares. Use it for what is worth keeping "
        "for later: the user's name, preferences, people, projects, or anything they ask you to remember. One "
        "short, complete fact each; never save what is already saved.",
        {"text": {"type": "string", "description": "The fact to remember."}},
        ["text"],
    ),
    tool(
        "forget",
        "Delete a saved memory, by its number in the list of memories.",
        {"number": {"type": "integer", "description": "The memory's number."}},
        ["number"],
    ),
    tool(
        "update_memory",
        "Replace the text of a saved memory, by its number in the list of memories.",
        {
            "number": {"type": "integer", "description": "The memory's number."},
            "text": {"type": "string", "description": "What it should say now."},
        },
        ["number", "text"],
    ),
    tool("list_memories", "List the saved memories with their numbers."),
]

VIEWER_TOOLS = [
    tool(
        "look",
        "Take a fresh look at what the viewer shows. It returns the item's details and gives you the picture, or "
        "the video's current frame, in the next message. Use it after changing the item or the video's position.",
    ),
    tool(
        "navigate",
        "Show another picture or video from the same folder or playlist.",
        {"to": {"type": "string", "enum": ["next", "previous", "first", "last"]}},
        ["to"],
    ),
    tool(
        "zoom",
        "Zoom the picture on the user's screen, or choose how it fits the window. This changes what the user sees; "
        "the picture you receive is always the whole frame, so you will not see the zoom.",
        {
            "action": {
                "type": "string",
                "enum": ["in", "out", "fit_window", "fit_width", "fit_height", "fill", "original_size"],
            },
            "steps": steps_property("zoom steps, for in and out"),
        },
        ["action"],
    ),
    tool(
        "pan",
        "Move the zoomed picture within the window on the user's screen. You will not see this.",
        {
            "direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
            "steps": steps_property("steps"),
        },
        ["direction"],
    ),
    tool(
        "rotate",
        "Rotate the picture or video on screen to an angle, clockwise from how the file has it.",
        {"degrees": {"type": "integer", "enum": [0, 90, 180, 270]}},
        ["degrees"],
    ),
    tool(
        "flip",
        "Mirror the picture or video on screen.",
        {"direction": {"type": "string", "enum": ["horizontal", "vertical"]}},
        ["direction"],
    ),
    tool(
        "playback",
        "Play a video that is paused, or pause one that is playing. It toggles, so use look afterwards if you need "
        "to know which it is now.",
    ),
    tool(
        "seek",
        "Move a video's position. Give seconds, or frames to step by, as a negative number to go back.",
        {
            "seconds": {"type": "number", "description": "Seconds to move by."},
            "frames": {"type": "integer", "description": "Frames to step by."},
        },
    ),
    tool(
        "speed",
        "Set a video's playback speed, such as 2 for double speed or 0.5 for half. 1 is normal.",
        {"speed": {"type": "number"}},
        ["speed"],
    ),
    tool(
        "volume",
        "Change a video's volume by a percentage, which is negative to turn it down, or mute or unmute it.",
        {
            "change_percent": {"type": "number", "description": "How much to change the volume by, in percent."},
            "toggle_mute": {"type": "boolean", "description": "Mute the sound, or unmute it."},
        },
    ),
    tool("fullscreen", "Switch the window between fullscreen and windowed."),
]

ZOOM_LABELS = {
    "in": "Zoom in",
    "out": "Zoom out",
    "fit_window": "Fit to window",
    "fit_width": "Fit to width",
    "fit_height": "Fit to height",
    "fill": "Fill the window",
    "original_size": "Original size",
}
ZOOM_ACTIONS = {
    "in": "image.zoom-in",
    "out": "image.zoom-out",
    "fit_window": "image.fit-window",
    "fit_width": "image.fit-width",
    "fit_height": "image.fit-height",
    "fill": "image.fit-fill",
    "original_size": "image.fit-original",
}
NAVIGATE_ACTIONS = {"next": "view.next", "previous": "view.previous", "first": "view.first", "last": "view.last"}


def parse_text_tool_calls(text):
    """Tool calls a model wrote into its reply as text. Returns (calls, the text without them).

    A server that does not turn the model's tool calls into structured ones
    leaves them in the reply, in the JSON form, ``<tool_call>{"name": ...,
    "arguments": {...}}</tool_call>``, or in the XML form of newer Qwen models,
    ``<tool_call><function=name><parameter=key>value</parameter></function></tool_call>``.
    """
    calls = []

    def take(match):
        body = match.group(1).strip()
        call = None
        try:
            value = json.loads(body)
            if isinstance(value, dict) and isinstance(value.get("name"), str):
                call = (value["name"], value.get("arguments", value.get("parameters", {})))
        except ValueError:
            function = re.search(r"<function=([\w.-]+)>(.*?)</function>", body, flags=re.S)
            if function:
                arguments = {}
                for key, raw in re.findall(r"<parameter=([\w.-]+)>\s*(.*?)\s*</parameter>", function.group(2), flags=re.S):
                    try:
                        arguments[key] = json.loads(raw)
                    except ValueError:
                        arguments[key] = raw
                call = (function.group(1), arguments)
        if call is None:
            return match.group(0)
        calls.append(call)
        return ""

    cleaned = re.sub(r"<tool_call>(.*?)</tool_call>", take, text, flags=re.S)
    return calls, cleaned


def describe_tools(specs):
    """The tools as a list in words, for a model that is told about them in its prompt."""
    lines = []
    for spec in specs:
        function = spec["function"]
        arguments = ", ".join(
            f"{key}: {'|'.join(map(str, value['enum'])) if 'enum' in value else value.get('type', 'any')}"
            for key, value in function["parameters"]["properties"].items()
        )
        lines.append(f"- {function['name']}({arguments}): {function['description']}")
    return "\n".join(lines)


PROMPTED_TOOLS = (
    "You can use tools. To use one, write a block like "
    '<tool_call>{"name": "navigate", "arguments": {"to": "next"}}</tool_call> in your reply and then stop; the '
    "result comes back in the next message. Tools:\n"
)

# Words that can come before or after a short command without changing it, as in "please show the next image".
QUICK_FILLER = {
    "please", "go", "to", "show", "me", "the", "a", "an", "open", "display", "view", "switch", "move", "skip",
    "now", "one", "item", "image", "picture", "photo", "pic", "video", "file", "media", "thanks", "thank", "you",
    "can", "could", "would", "just", "and", "then", "it", "i", "want", "see", "lets", "let", "hey", "ok", "okay", "s",
}
QUICK_COMMANDS = {
    ("next",): ("navigate", {"to": "next"}),
    ("previous",): ("navigate", {"to": "previous"}),
    ("prev",): ("navigate", {"to": "previous"}),
    ("back",): ("navigate", {"to": "previous"}),
    ("first",): ("navigate", {"to": "first"}),
    ("last",): ("navigate", {"to": "last"}),
    ("play",): ("playback", {}),
    ("pause",): ("playback", {}),
    ("resume",): ("playback", {}),
    ("zoom", "in"): ("zoom", {"action": "in"}),
    ("zoom", "out"): ("zoom", {"action": "out"}),
    ("fullscreen",): ("fullscreen", {}),
    ("full", "screen"): ("fullscreen", {}),
    ("mute",): ("volume", {"toggle_mute": True}),
    ("unmute",): ("volume", {"toggle_mute": True}),
}


def quick_command(text):
    """The tool a short command like "next image" or "show next" means, or None for anything else.

    Only a message that is nothing but the command, perhaps with polite or
    filler words round it, counts. "What is the next image in this folder?"
    goes to the model.
    """
    words = re.findall(r"[a-z]+", text.lower())
    if not words or len(words) > 8:
        return None
    return QUICK_COMMANDS.get(tuple(word for word in words if word not in QUICK_FILLER))


# -- reasoning -----------------------------------------------------------


class ThinkSplitter:
    """Separates ``<think>`` blocks from a reply that arrives in pieces."""

    OPEN = "<think>"
    CLOSE = "</think>"

    def __init__(self):
        self.thinking = False
        self.buffer = ""

    def feed(self, text):
        """Returns (is_thinking, text) segments. A tag split across pieces is held back until it is whole."""
        self.buffer += text
        segments = []
        while True:
            tag = self.CLOSE if self.thinking else self.OPEN
            index = self.buffer.find(tag)
            if index >= 0:
                if index:
                    segments.append((self.thinking, self.buffer[:index]))
                self.buffer = self.buffer[index + len(tag):]
                self.thinking = not self.thinking
                continue

            held = 0
            for length in range(min(len(tag) - 1, len(self.buffer)), 0, -1):
                if tag.startswith(self.buffer[-length:]):
                    held = length
                    break
            ready = self.buffer[: len(self.buffer) - held]
            if ready:
                segments.append((self.thinking, ready))
            self.buffer = self.buffer[len(self.buffer) - held:]
            return segments

    def flush(self):
        segments = [(self.thinking, self.buffer)] if self.buffer else []
        self.buffer = ""
        return segments


class Request:
    """One chat request. Replies from a request that is no longer current are ignored."""

    def __init__(self):
        self.cancel = threading.Event()
        self.thinking = []
        self.reply = []
        self.splitter = ThinkSplitter()
        # Whether any of the response has been shown yet.
        self.arrived = False
        # The conversation sent so far, which each round of tool calls adds to.
        self.messages = []
        # The tool calls of the round being read, by their index, as they arrive.
        self.calls = {}
        # What the tools did, for the chat to show.
        self.notes = []
        self.rounds = 0
        # Where the reply of the round now being read starts, in `reply`.
        self.round_start = 0
        # A picture to show the model after the round's tool results: (viewer details, base64 PNG).
        self.look = None
        # Whether this round's request carried the tools, and the reply of a round as the model wrote it.
        self.sent_tools = False
        self.round_raw = ""

    def add(self, is_thinking, text):
        (self.thinking if is_thinking else self.reply).append(text)

    def feed(self, text):
        for is_thinking, piece in self.splitter.feed(text):
            self.add(is_thinking, piece)

    def finish(self):
        for is_thinking, piece in self.splitter.flush():
            self.add(is_thinking, piece)

    def texts(self):
        return "".join(self.thinking), "".join(self.reply)


class Speech:
    """One piece of speech being synthesized or played."""

    def __init__(self):
        self.cancelled = False
        self.proc = None
        self.lock = threading.Lock()

    def stop(self):
        with self.lock:
            self.cancelled = True
            if self.proc is not None and self.proc.poll() is None:
                self.proc.terminate()


class Listener:
    """Listens to the microphone and reports each stretch of speech once the speaker pauses.

    The microphone program writes raw 16 kHz mono 16-bit audio to its standard
    output. Sound counts as speech when it is louder than both the configured
    threshold and a few times the room's own noise level, which is tracked as it
    changes. Listening happens only while ``gate`` is set, and the gate is closed
    when speech is reported, so the reply and its spoken version are not heard.
    """

    RATE = 16000
    FRAME_MS = 30
    FRAME_BYTES = RATE * FRAME_MS // 1000 * 2
    # Frames of sound in a row before it counts as speech.
    START_FRAMES = 4
    # Frames kept from before the start, so the first word is not clipped.
    PREROLL_FRAMES = 10
    # Frames of the pause kept after the last word.
    TAIL_FRAMES = 6
    # Less speech than this is a click or a cough, not a message.
    MIN_VOICED_MS = 300
    MAX_MS = 30000
    # Seconds ignored after listening resumes, so the end of a spoken reply is not heard.
    HOLDOFF = 0.5
    # While a reply is being spoken the speakers' own sound reaches the microphone, so speech must be this much
    # louder, and last twice as long, to count.
    BARGE_FACTOR = 2.5

    def __init__(self, args, threshold, silence_ms, on_hearing, on_speech, on_ended):
        self.threshold = threshold
        self.silence_ms = silence_ms
        self.on_hearing = on_hearing
        self.on_speech = on_speech
        self.on_ended = on_ended
        self.gate = threading.Event()
        # Set while a reply is being spoken.
        self.speaking = threading.Event()
        self.stopped = threading.Event()
        self.proc = subprocess.Popen(
            args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
        )
        threading.Thread(target=self._run, daemon=True).start()

    def stop(self):
        self.stopped.set()
        self.gate.clear()
        if self.proc.poll() is None:
            self.proc.terminate()

    @staticmethod
    def level(frame):
        samples = array.array("h", frame)
        return math.sqrt(sum(sample * sample for sample in samples) / len(samples))

    def _run(self):
        stream = self.proc.stdout
        floor = 150.0
        preroll = collections.deque(maxlen=self.PREROLL_FRAMES)
        speech = None
        loud = voiced = silent = 0
        was_open = False
        resume = 0.0
        try:
            while not self.stopped.is_set():
                frame = stream.read(self.FRAME_BYTES)
                if len(frame) < self.FRAME_BYTES:
                    break

                if not self.gate.is_set():
                    was_open, speech, loud = False, None, 0
                    preroll.clear()
                    continue
                now = time.monotonic()
                if not was_open:
                    was_open = True
                    resume = now + self.HOLDOFF
                if now < resume:
                    continue

                level = self.level(frame)
                speaking = self.speaking.is_set()
                # Over a reply the bar is raised, and the noise level is not learned, or the speakers' own sound
                # would be taken for the room's and no voice could get above it.
                threshold = max(self.threshold * (self.BARGE_FACTOR if speaking else 1), floor * 3)
                start_frames = self.START_FRAMES * (2 if speaking else 1)
                if speech is None:
                    preroll.append(frame)
                    if level >= threshold:
                        loud += 1
                    else:
                        loud = 0
                        if not speaking:
                            floor = min(2000.0, max(20.0, floor * 0.98 + level * 0.02))
                    if loud >= start_frames:
                        speech, voiced, silent = list(preroll), loud, 0
                        self.on_hearing(True)
                    continue

                speech.append(frame)
                if level >= threshold * 0.6:
                    voiced += 1
                    silent = 0
                else:
                    silent += 1
                if silent * self.FRAME_MS < self.silence_ms and len(speech) * self.FRAME_MS < self.MAX_MS:
                    continue

                end = len(speech) - max(0, silent - self.TAIL_FRAMES)
                pcm = b"".join(speech[:end])
                enough = voiced * self.FRAME_MS >= self.MIN_VOICED_MS
                speech, loud = None, 0
                preroll.clear()
                if enough:
                    # Nothing more is heard until the reply has been given and spoken.
                    self.gate.clear()
                    self.on_speech(pcm)
                else:
                    self.on_hearing(False)
        finally:
            if self.proc.poll() is None:
                self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            if not self.stopped.is_set():
                self.on_ended()


# -- widgets -------------------------------------------------------------


class Bubble:
    """One message in a panel's log, drawn as a speech bubble: yours on the right, the model's on the left.

    Under the bubble is a row of actions, shown while the pointer is over the
    message. The row always has its height and is only faded, and the bubble's
    text wraps at a fixed number of characters, so nothing about a message
    changes size when the pointer moves or a reply finishes.
    """

    ACTIONS_HEIGHT = 26
    # The text wraps at about this many characters, or at the panel's width if that is narrower.
    MAX_CHARS = 60

    def __init__(
        self, role, text, thinking, name, plugin, live=False, message_id=None, thumb=None, path=None, notes=None
    ):
        self.role = role
        self.message_id = message_id
        # A live bubble is being written now, so it can say how long the model thought.
        self._live = live
        self.text = text
        self.thinking = thinking
        self.failed = False
        self._started = time.monotonic()
        self._thinking_shown = False
        self._collapsed = False
        user = role == "user"
        side = Gtk.Align.END if user else Gtk.Align.START

        # The whole message: who it is from, the bubble, and its actions. It fills the panel's width.
        self.box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        self.box.set_margin_start(36 if user else 6)
        self.box.set_margin_end(6 if user else 36)
        motion = Gtk.EventControllerMotion()
        motion.connect("enter", lambda controller, x, y: self.set_hover(True))
        motion.connect("leave", lambda controller: self.set_hover(False))
        self.box.add_controller(motion)

        self.caption = Gtk.Label(label=name, xalign=1.0 if user else 0.0)
        self.caption.add_css_class("caption")
        self.caption.set_opacity(0.7)
        self.caption.set_ellipsize(Pango.EllipsizeMode.END)
        self.caption.set_margin_start(8)
        self.caption.set_margin_end(8)

        bubble = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        bubble.set_halign(side)
        bubble.add_css_class("llm-bubble")
        bubble.add_css_class("llm-bubble-user" if user else "llm-bubble-assistant")

        # The picture that was sent with a message of yours, which opens in the viewer when clicked.
        texture = thumbnail_texture(thumb) if user and thumb else None
        if texture is not None:
            picture = Gtk.Picture.new_for_paintable(texture)
            picture.set_halign(Gtk.Align.START)
            picture.set_can_shrink(False)
            picture.set_overflow(Gtk.Overflow.HIDDEN)
            picture.add_css_class("llm-thumb")
            if path:
                picture.set_cursor_from_name("pointer")
                picture.set_tooltip_text(f"Open {os.path.basename(path)} in the viewer")
                click = Gtk.GestureClick()
                click.set_button(1)
                click.connect("released", lambda gesture, presses, x, y: plugin._open_in_viewer(path))
                picture.add_controller(click)
            bubble.append(picture)

        self.think_label = Gtk.Label(xalign=0.0)
        self.think_label.set_wrap(True)
        self.think_label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.think_label.set_max_width_chars(self.MAX_CHARS)
        self.think_label.set_selectable(True)
        self.think_label.set_opacity(0.7)
        self.expander = Gtk.Expander(label="Thinking…")
        self.expander.set_child(self.think_label)
        self.expander.set_visible(False)

        # What the model did with its tools, such as zooming or saving a memory.
        self.notes_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=1)
        for note in notes or []:
            self.add_note(note)

        self.body = Gtk.Label(xalign=0.0)
        self.body.set_wrap(True)
        self.body.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.body.set_max_width_chars(self.MAX_CHARS)
        self.body.set_selectable(True)

        bubble.append(self.expander)
        bubble.append(self.notes_box)
        bubble.append(self.body)

        self.replay_button = Gtk.Button.new_from_icon_name("audio-volume-high-symbolic")
        self.regenerate_button = Gtk.Button.new_from_icon_name("view-refresh-symbolic")
        self.delete_button = Gtk.Button.new_from_icon_name("user-trash-symbolic")
        self.actions = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        self.actions.set_halign(side)
        self.actions.set_size_request(-1, self.ACTIONS_HEIGHT)
        self.actions.set_opacity(0.0)
        for button, tip, callback, shown in (
            (self.replay_button, "Read aloud", lambda *_: plugin._replay(self), not user),
            (self.regenerate_button, "Regenerate", lambda *_: plugin._regenerate(), not user),
            (
                self.delete_button,
                "Delete this message and its reply" if user else "Delete this reply",
                lambda *_: plugin._delete(self),
                True,
            ),
        ):
            button.add_css_class("flat")
            button.add_css_class("circular")
            button.add_css_class("llm-action")
            button.set_valign(Gtk.Align.CENTER)
            button.set_focusable(False)
            button.set_tooltip_text(tip)
            button.connect("clicked", callback)
            if shown:
                self.actions.append(button)
        self.set_actions()

        self.box.append(self.caption)
        self.box.append(bubble)
        self.box.append(self.actions)
        self.show(thinking, text, final=True)

    def set_hover(self, hovered):
        """Fades the actions in while the pointer is over the message. Their room is always kept."""
        self.actions.set_opacity(1.0 if hovered else 0.0)

    def set_actions(self, replay=False, regenerate=False, delete=False):
        """Chooses which actions the message has. The row keeps its height whichever there are."""
        self.replay_button.set_visible(replay)
        self.regenerate_button.set_visible(regenerate)
        self.delete_button.set_visible(delete)

    def add_note(self, text):
        label = Gtk.Label(label=f"⚙ {text}", xalign=0.0)
        label.set_wrap(True)
        label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        label.set_opacity(0.7)
        label.add_css_class("caption")
        self.notes_box.append(label)

    def show(self, thinking, reply, final=False, failed=False):
        """Shows the reasoning and the reply so far. The reasoning collapses when the reply starts."""
        self.thinking, self.text, self.failed = thinking, reply, failed

        if thinking:
            self.expander.set_visible(True)
            self.think_label.set_text(thinking)
            if not self._thinking_shown:
                self._thinking_shown = True
                self.expander.set_expanded(not (reply or final))
        if (reply or final) and thinking and not self._collapsed:
            self._collapsed = True
            self.expander.set_expanded(False)
            seconds = time.monotonic() - self._started
            self.expander.set_label(f"Thought for {seconds:.0f} s" if self._live else "Thought")

        if self.role == "user":
            self.body.set_text(reply)
        elif failed:
            self.body.set_text(reply)
            self.body.add_css_class("error")
        elif reply:
            self.body.set_visible(True)
            self.body.set_markup(markdown_markup(reply))
        elif thinking and not final:
            self.body.set_visible(False)
        else:
            self.body.set_visible(True)
            self.body.set_text("" if final else "…")


class Panel:
    """The widgets of one chat panel. Each window has its own."""

    def __init__(self, panel_id, instance_id):
        self.key = (panel_id, instance_id)
        self.in_viewer = panel_id == "viewer-chat"
        self.container = None
        self.file_label = None
        self.scroll = None
        self.log = None
        self.status = None
        self.entry = None
        self.send_button = None
        self.mic_button = None
        self.speak_button = None
        self.voice_button = None
        self.thinking_button = None
        self.memory_button = None
        self.memory_list = None
        self.memory_entry = None
        self.stop_speaking_button = None
        self.bubbles = []
        self.streaming = None
        # Whether new messages scroll into view. It is off while the reader is scrolled up.
        self.follow = True
        # The log's size when its scrollbar last changed, to tell the reader scrolling from the log growing.
        self.upper = 0.0
        self.page = 0.0
        # The button over the log that goes to the latest message, shown while the log is not at its end.
        self.to_bottom = None
        # Frames still to pin the log to its end, and whether a frame callback is running for that.
        self.settle = 0
        self.ticking = False
        # Whether the viewer card is placed by margins from the top left rather than by the host's corner.
        self.anchored = False
        # The place and size the user chose for the viewer card: x, y (once placed), w and h.
        self.want = None
        # The player's size the card was last fitted to, to fit it again when the window changes.
        self.fitted = None
        # Whether the viewer card is shown or on its way to be, and the callback fading it.
        self.shown = True
        self.fade = None


class Plugin:
    def __init__(self):
        self._active = True
        self._generation = None
        self._config = dict(DEFAULTS)
        self._history_path = None
        self._layout_path = None
        self._history = []
        self._panels = {}
        self._request = None
        self._last_image = None
        self._speech = None
        self._speak = False
        self._recorder = None
        self._record_dir = None
        self._record_panel = None
        self._transcribing = False
        self._chat_shown = True
        # The listener exists while a voice conversation is on.
        self._listener = None
        self._voice_busy = False
        self._hearing = False
        # What was last spoken and when it stopped, to tell a transcript of the speakers from the user.
        self._last_spoken = ""
        self._spoken_at = 0.0
        self._over_speech = False
        self._config_file = None
        # Set when the server turns out not to accept tools, so they are described in the prompt instead.
        self._text_tools = False
        self._memory_path = None
        self._memories = []
        # The memories as numbered in the last request, which is how the model names one to forget.
        self._memory_ids = []
        self._thinking = True
        # The request parameters, parsed from the settings when the plugin loads.
        self._temperature = None
        self._top_p = None
        self._extra_params = {}
        self._thinking_on = {}
        self._thinking_off = {}
        self._card_style = None

    def activate(self, host):
        self._active = True
        self._generation = rsimgv.host.generation
        config_path = rsimgv.host.config_path()
        directory = os.path.dirname(config_path)
        self._history_path = os.path.join(directory, "history.json")
        self._layout_path = os.path.join(directory, "layout.json")
        self._config_file = os.path.join(directory, "config.json")
        self._memory_path = os.path.join(directory, "memory.json")
        self._config = self._load_config(config_path)
        self._parse_parameters()
        self._speak = bool(self._config["auto_speak"]) and self._config["tts_backend"] != "off"
        self._history = self._load_history()
        self._memories = self._load_memories()

    def deactivate(self):
        # Work in flight cannot be joined safely. Its results are ignored when
        # they arrive, because the active flag and generation no longer match.
        self._active = False
        if self._request is not None:
            self._request.cancel.set()
            self._request = None
        self._stop_speech()
        self._abandon_recording()
        self._end_voice()
        self._remove_card_style()
        self._panels = {}

    # -- host callbacks --------------------------------------------------

    def invoke_action(self, action, context):
        if action == "toggle-chat":
            self._toggle_chat()
        elif action == "clear-chat":
            self._clear()
        elif action == "voice-mode":
            self._toggle_voice()
        elif action == "speak-toggle":
            self._toggle_speak()
        elif action == "stop-speaking":
            self._stop_speech()
        else:
            return
        rsimgv.action_done({"result": "ok"})

    def on_event(self, name, data):
        if name != "viewer.item_changed":
            return
        path = data.get("path") if isinstance(data, dict) else None
        self._show_file(os.path.basename(path) if path else "")

    def create_panel(self, panel_id, instance_id, container, context):
        if panel_id not in ("chat", "viewer-chat"):
            return

        panel = Panel(panel_id, instance_id)
        panel.container = container
        config = self._config
        self._ensure_style()

        panel.file_label = Gtk.Label(xalign=0.0, hexpand=True)
        panel.file_label.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        panel.file_label.add_css_class("dim-label")
        panel.file_label.add_css_class("caption")

        panel.speak_button = Gtk.Button.new_from_icon_name("audio-volume-muted-symbolic")
        panel.speak_button.add_css_class("flat")
        panel.speak_button.set_visible(config["tts_backend"] != "off")

        panel.thinking_button = Gtk.Button(label="Think")
        panel.thinking_button.add_css_class("flat")
        panel.thinking_button.add_css_class("caption")
        panel.thinking_button.set_visible(bool(self._thinking_on or self._thinking_off))

        panel.voice_button = Gtk.Button.new_from_icon_name("audio-headset-symbolic")
        panel.voice_button.add_css_class("flat")
        panel.voice_button.set_visible(config["asr_backend"] != "off")

        panel.stop_speaking_button = Gtk.Button.new_from_icon_name("media-playback-stop-symbolic")
        panel.stop_speaking_button.add_css_class("flat")
        panel.stop_speaking_button.set_tooltip_text("Stop speaking")
        panel.stop_speaking_button.set_visible(False)

        clear_button = Gtk.Button.new_from_icon_name("edit-clear-all-symbolic")
        clear_button.add_css_class("flat")
        clear_button.set_tooltip_text("Clear the chat. Memories are kept.")

        panel.memory_button = Gtk.MenuButton()
        panel.memory_button.set_icon_name("user-bookmarks-symbolic")
        panel.memory_button.add_css_class("flat")
        panel.memory_button.set_tooltip_text("Memories, shared by every chat")
        panel.memory_button.set_visible(config["memory"])
        panel.memory_list = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        memory_scroll = Gtk.ScrolledWindow()
        memory_scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        memory_scroll.set_propagate_natural_height(True)
        memory_scroll.set_max_content_height(260)
        memory_scroll.set_min_content_width(300)
        memory_scroll.set_child(panel.memory_list)
        panel.memory_entry = Gtk.Entry(hexpand=True)
        panel.memory_entry.set_placeholder_text("Add a memory…")
        memory_add = Gtk.Button.new_from_icon_name("list-add-symbolic")
        memory_add.set_tooltip_text("Add this memory")
        memory_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
        memory_row.append(panel.memory_entry)
        memory_row.append(memory_add)
        memory_title = Gtk.Label(label="Memories", xalign=0.0)
        memory_title.add_css_class("heading")
        memory_note = Gtk.Label(
            label="Shared by every chat. Ask the model to remember or forget something.", xalign=0.0
        )
        memory_note.set_wrap(True)
        memory_note.set_max_width_chars(38)
        memory_note.add_css_class("dim-label")
        memory_note.add_css_class("caption")
        memory_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        for margin in ("top", "bottom", "start", "end"):
            getattr(memory_box, f"set_margin_{margin}")(10)
        for child in (memory_title, memory_scroll, memory_row, memory_note):
            memory_box.append(child)
        popover = Gtk.Popover()
        popover.set_child(memory_box)
        panel.memory_button.set_popover(popover)

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        header.append(panel.file_label)
        header.append(panel.stop_speaking_button)
        header.append(panel.thinking_button)
        header.append(panel.voice_button)
        header.append(panel.speak_button)
        header.append(panel.memory_button)
        header.append(clear_button)

        panel.log = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        panel.log.set_margin_top(4)
        panel.log.set_margin_bottom(4)

        panel.scroll = Gtk.ScrolledWindow(vexpand=True)
        # The scrollbar is always there. With one that comes and goes, its arrival narrows the log, the wrapped
        # messages re-wrap and grow, and the height measured a moment before is too small, so the last of the log
        # stays out of reach until something makes it measure again.
        panel.scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.ALWAYS)
        panel.scroll.set_min_content_height(80 if panel.in_viewer else 260)
        panel.scroll.set_child(panel.log)

        panel.status = Gtk.Label(xalign=0.0, hexpand=True)
        panel.status.add_css_class("dim-label")
        panel.status.add_css_class("caption")
        # The line is always there, empty when there is nothing to say, so it appearing does not resize the log.
        panel.status.set_text(" ")

        panel.mic_button = Gtk.Button.new_from_icon_name("audio-input-microphone-symbolic")
        panel.mic_button.set_tooltip_text("Speak a message")
        panel.mic_button.set_visible(config["asr_backend"] != "off")

        panel.entry = Gtk.Entry(hexpand=True)
        panel.entry.set_placeholder_text("Ask about this picture…")

        panel.send_button = Gtk.Button.new_from_icon_name("mail-send-symbolic")
        panel.send_button.add_css_class("suggested-action")
        panel.send_button.set_tooltip_text("Send")

        row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
        row.append(panel.mic_button)
        row.append(panel.entry)
        row.append(panel.send_button)

        footer = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
        footer.append(panel.status)

        content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        content.append(header)
        # A button over the log goes to the latest message while the log is scrolled away from it.
        panel.to_bottom = Gtk.Button.new_from_icon_name("go-down-symbolic")
        panel.to_bottom.add_css_class("circular")
        panel.to_bottom.add_css_class("osd")
        panel.to_bottom.set_halign(Gtk.Align.CENTER)
        panel.to_bottom.set_valign(Gtk.Align.END)
        panel.to_bottom.set_margin_bottom(10)
        panel.to_bottom.set_tooltip_text("Go to the latest message")
        panel.to_bottom.set_visible(False)
        scroll_area = Gtk.Overlay(vexpand=True)
        scroll_area.set_child(panel.scroll)
        scroll_area.add_overlay(panel.to_bottom)
        content.append(scroll_area)
        content.append(footer)
        content.append(row)

        corners = {}
        if panel.in_viewer:
            # Each corner has a small handle over the content that resizes the card when it is dragged.
            frame = Gtk.Overlay(vexpand=True)
            frame.set_child(content)
            for name, (horizontal, vertical, cursor) in CORNERS.items():
                corner = Gtk.Box()
                corner.set_size_request(CORNER_SIZE, CORNER_SIZE)
                corner.set_halign(horizontal)
                corner.set_valign(vertical)
                corner.set_cursor_from_name(cursor)
                frame.add_overlay(corner)
                corners[name] = corner
            container.append(frame)
        else:
            container.append(content)

        rsimgv.ui.connect(panel.send_button, "clicked", lambda *_: self._send_or_stop(panel))
        rsimgv.ui.connect(panel.entry, "activate", lambda *_: self._send_from(panel))
        rsimgv.ui.connect(panel.mic_button, "clicked", lambda *_: self._toggle_record(panel))
        rsimgv.ui.connect(panel.speak_button, "clicked", lambda *_: self._toggle_speak())
        rsimgv.ui.connect(panel.voice_button, "clicked", lambda *_: self._toggle_voice())
        rsimgv.ui.connect(panel.thinking_button, "clicked", lambda *_: self._toggle_thinking())
        rsimgv.ui.connect(panel.stop_speaking_button, "clicked", lambda *_: self._stop_speech())
        rsimgv.ui.connect(clear_button, "clicked", lambda *_: self._clear())
        rsimgv.ui.connect(panel.to_bottom, "clicked", lambda *_: self._scroll_down(panel))
        rsimgv.ui.connect(memory_add, "clicked", lambda *_: self._add_memory_from(panel))
        rsimgv.ui.connect(panel.memory_entry, "activate", lambda *_: self._add_memory_from(panel))

        # The log follows new messages while it is at the bottom, and stays put once the reader scrolls up.
        adjustment = panel.scroll.get_vadjustment()
        rsimgv.ui.connect(adjustment, "value-changed", lambda a: self._scrolled(panel, a))
        rsimgv.ui.connect(adjustment, "changed", lambda a: self._grew(panel, a))
        # A size change is also reported as a property change, which does not depend on how the size was set.
        rsimgv.ui.connect(adjustment, "notify::upper", lambda a, _spec: self._grew(panel, a))
        rsimgv.ui.connect(adjustment, "notify::page-size", lambda a, _spec: self._grew(panel, a))
        rsimgv.ui.timeout_add(150, lambda: self._watch(panel))

        if panel.in_viewer:
            container.add_css_class("llm-chat-card")
            self._place_card(panel, header, corners)
            container.set_visible(self._chat_shown)
            panel.shown = self._chat_shown
            # Room round the content, so the corner handles are not over its buttons.
            for child in (header, scroll_area, footer, row):
                child.set_margin_start(CORNER_SIZE)
                child.set_margin_end(CORNER_SIZE)
            header.set_margin_top(CORNER_SIZE)
            row.set_margin_bottom(CORNER_SIZE)
        else:
            # The chat sidebar is open or closed with its panel, and the panel fills its height.
            container.set_vexpand(True)
            container.set_visible(config["chat_sidebar"])
            for margin in ("top", "bottom", "start", "end"):
                getattr(container, f"set_margin_{margin}")(8)

        self._panels[panel.key] = panel
        self._refresh_memory()
        for message in self._history:
            self._add_bubble(
                panel, message["role"], message["text"], message.get("file"), message.get("thinking", ""),
                message=message,
            )
        self._refresh_controls()

        try:
            info = rsimgv.host.call("view.info")
            self._show_file(info.get("filename") or "")
        except Exception:
            self._show_file("")

    def panel_destroyed(self, panel_id, instance_id):
        self._panels.pop((panel_id, instance_id), None)

    # -- the viewer card: moving and resizing ----------------------------

    def _place_card(self, panel, header, corners):
        """Sets the card's saved size and place, and lets its header move it and any of its corners resize it.

        The geometry the user chose is kept in ``panel.want``. What is on screen is that geometry fitted to the
        player's current size by ``_fit_card``, so a small window does not lose the saved place.
        """
        container = panel.container
        layout = self._load_layout()
        panel.want = {
            "w": max(CARD_MIN[0], layout.get("w", CARD_SIZE[0])),
            "h": max(CARD_MIN[1], layout.get("h", CARD_SIZE[1])),
        }
        container.set_size_request(panel.want["w"], panel.want["h"])
        if "x" in layout and "y" in layout:
            panel.want["x"], panel.want["y"] = layout["x"], layout["y"]
            self._anchor(panel, layout["x"], layout["y"])
            # The player's size is not known until it is shown. _watch fits the card then, and when the size changes.

        header.set_cursor_from_name("move")
        move = Gtk.GestureDrag()
        move.set_button(1)
        state = {}

        def move_begin(gesture, x, y):
            state.clear()
            overlay = container.get_parent()
            if overlay is not None:
                state["local"] = (x, y)
                state["pointer"] = self._in_overlay(header, overlay, x, y)

        def move_update(gesture, dx, dy):
            overlay = container.get_parent()
            if overlay is None or "pointer" not in state:
                return
            if "start" not in state:
                # Anchored on the first movement, so a click that does not drag changes nothing.
                self._anchor_here(panel)
                state["start"] = (panel.want["x"], panel.want["y"])
            px, py = self._in_overlay(header, overlay, state["local"][0] + dx, state["local"][1] + dy)
            panel.want["x"] = state["start"][0] + px - state["pointer"][0]
            panel.want["y"] = state["start"][1] + py - state["pointer"][1]
            self._fit_card(panel)
            # What is shown is the wanted place fitted to the player, which is what the card is dropped at.
            panel.want["x"], panel.want["y"] = container.get_margin_start(), container.get_margin_top()

        def move_end(gesture, dx, dy):
            if "start" in state:
                self._save_layout(panel)
            state.clear()

        rsimgv.ui.connect(move, "drag-begin", move_begin)
        rsimgv.ui.connect(move, "drag-update", move_update)
        rsimgv.ui.connect(move, "drag-end", move_end)
        header.add_controller(move)

        # A double click puts the card back in its corner at its first size.
        reset = Gtk.GestureClick()
        reset.set_button(1)
        rsimgv.ui.connect(
            reset, "pressed", lambda g, presses, x, y: self._reset_card(panel) if presses == 2 else None
        )
        header.add_controller(reset)

        for name, corner in corners.items():
            self._attach_resize(panel, corner, left=name.endswith("left"), top=name.startswith("top"))

    def _attach_resize(self, panel, corner, left, top):
        """Lets dragging `corner` resize the card. A left or top corner also moves the card, to keep the far edge still."""
        container = panel.container
        resize = Gtk.GestureDrag()
        resize.set_button(1)
        state = {}

        def begin(gesture, x, y):
            state.clear()
            overlay = container.get_parent()
            if overlay is not None:
                state["local"] = (x, y)
                state["pointer"] = self._in_overlay(corner, overlay, x, y)

        def update(gesture, dx, dy):
            overlay = container.get_parent()
            if overlay is None or "pointer" not in state:
                return
            if "origin" not in state:
                self._anchor_here(panel)
                state["origin"] = (panel.want["x"], panel.want["y"])
                state["size"] = (panel.want["w"], panel.want["h"])
            px, py = self._in_overlay(corner, overlay, state["local"][0] + dx, state["local"][1] + dy)
            moved_x, moved_y = px - state["pointer"][0], py - state["pointer"][1]
            x0, y0 = state["origin"]
            w0, h0 = state["size"]
            if left:
                width = max(CARD_MIN[0], min(w0 - moved_x, x0 + w0))
                x = x0 + w0 - width
            else:
                width = max(CARD_MIN[0], min(w0 + moved_x, overlay.get_width() - x0))
                x = x0
            if top:
                height = max(CARD_MIN[1], min(h0 - moved_y, y0 + h0))
                y = y0 + h0 - height
            else:
                height = max(CARD_MIN[1], min(h0 + moved_y, overlay.get_height() - y0))
                y = y0
            panel.want.update(x=int(x), y=int(y), w=int(width), h=int(height))
            self._fit_card(panel)

        def end(gesture, dx, dy):
            if "origin" in state:
                self._save_layout(panel)
            state.clear()

        rsimgv.ui.connect(resize, "drag-begin", begin)
        rsimgv.ui.connect(resize, "drag-update", update)
        rsimgv.ui.connect(resize, "drag-end", end)
        corner.add_controller(resize)

    @staticmethod
    def _in_overlay(widget, overlay, x, y):
        """A point of `widget`, in the coordinates of `overlay`, which does not move while the card does."""
        point = Graphene.Point()
        point.init(x, y)
        ok, result = widget.compute_point(overlay, point)
        return (result.x, result.y) if ok else (x, y)

    def _anchor(self, panel, x, y):
        container = panel.container
        container.set_halign(Gtk.Align.START)
        container.set_valign(Gtk.Align.START)
        container.set_margin_start(int(x))
        container.set_margin_top(int(y))
        container.set_margin_end(0)
        container.set_margin_bottom(0)
        panel.anchored = True

    def _anchor_here(self, panel):
        """Places the card by margins from the top left, where it is now, so it can be moved freely."""
        if panel.anchored:
            return
        container = panel.container
        overlay = container.get_parent()
        x, y = self._in_overlay(container, overlay, 0, 0) if overlay is not None else (12, 12)
        width, height = container.get_width(), container.get_height()
        self._anchor(panel, x, y)
        panel.want.update(
            x=int(x), y=int(y),
            w=max(CARD_MIN[0], width or panel.want["w"]), h=max(CARD_MIN[1], height or panel.want["h"]),
        )
        container.set_size_request(panel.want["w"], panel.want["h"])

    def _fit_card(self, panel):
        """Shows the card's wanted size and place inside the player, which it never leaves.

        Nothing is done until the player has a size. The wanted geometry itself is not changed, so a card pushed in
        by a small window goes back to its place when the window grows again.
        """
        container, want = panel.container, panel.want
        overlay = container.get_parent()
        if overlay is None or want is None:
            return
        room_w, room_h = overlay.get_width(), overlay.get_height()
        if room_w <= 0 or room_h <= 0:
            return
        # The default corner leaves its margin all round; a placed card has none.
        margin = 0 if panel.anchored else 12
        width = max(1, min(max(CARD_MIN[0], want["w"]), room_w - 2 * margin))
        height = max(1, min(max(CARD_MIN[1], want["h"]), room_h - 2 * margin))
        if container.get_size_request() != (width, height):
            container.set_size_request(width, height)
        if panel.anchored and "x" in want:
            # A card cannot be smaller than its content, so keep what it needs inside, not what was asked for.
            shown_w = max(width, container.measure(Gtk.Orientation.HORIZONTAL, -1)[0])
            shown_h = max(height, container.measure(Gtk.Orientation.VERTICAL, shown_w)[0])
            x = int(max(0, min(want["x"], room_w - shown_w)))
            y = int(max(0, min(want["y"], room_h - shown_h)))
            if container.get_margin_start() != x:
                container.set_margin_start(x)
            if container.get_margin_top() != y:
                container.set_margin_top(y)
        panel.fitted = (room_w, room_h)

    def _reset_card(self, panel):
        container = panel.container
        container.set_halign(Gtk.Align.END)
        container.set_valign(Gtk.Align.START)
        for margin in ("start", "top", "end", "bottom"):
            getattr(container, f"set_margin_{margin}")(12)
        container.set_size_request(*CARD_SIZE)
        panel.anchored = False
        panel.want = {"w": CARD_SIZE[0], "h": CARD_SIZE[1]}
        panel.fitted = None
        try:
            os.remove(self._layout_path)
        except FileNotFoundError:
            pass
        except OSError as exc:
            rsimgv.notify(f"{NAME}: cannot reset the chat's place: {exc}", level="warning", seconds=6)

    def _load_layout(self):
        try:
            with open(self._layout_path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
        except (OSError, ValueError):
            return {}
        if not isinstance(loaded, dict):
            return {}
        layout = {
            key: int(value)
            for key, value in loaded.items()
            if key in ("x", "y", "w", "h") and isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value)
        }
        # A place without a usable size, or off the top left, is not trusted.
        if "x" in layout and "y" in layout and (layout["x"] < 0 or layout["y"] < 0):
            layout.pop("x"), layout.pop("y")
        return layout

    def _save_layout(self, panel):
        """Writes the wanted place and size. The file is replaced whole, so a crash cannot leave half of it."""
        want = panel.want
        if not want or "x" not in want:
            return
        layout = {key: int(want[key]) for key in ("x", "y", "w", "h")}
        temporary = f"{self._layout_path}.tmp"
        try:
            os.makedirs(os.path.dirname(self._layout_path), exist_ok=True)
            with open(temporary, "w", encoding="utf-8") as file:
                json.dump(layout, file)
            os.replace(temporary, self._layout_path)
        except OSError as exc:
            rsimgv.notify(f"{NAME}: cannot save the chat's place: {exc}", level="warning", seconds=6)

    # -- the viewer card: fading ------------------------------------------

    def _fade_ms(self):
        """How long the card takes to fade, or 0 while the window's transitions are off."""
        try:
            if not rsimgv.host.call("settings.get", {"key": "window.sidebar.animations"}).get("value"):
                return 0
            return max(0, int(rsimgv.host.call("settings.get", {"key": "window.sidebar.animation-ms"}).get("value")))
        except Exception as exc:
            if DEBUG:
                rsimgv.host.log("debug", f"chat card: cannot read the transition settings: {exc}")
            return 0

    def _show_card(self, panel, show):
        """Shows or hides the viewer card, fading it when the window's transitions are on."""
        container = panel.container
        if panel.fade is not None:
            container.remove_tick_callback(panel.fade)
            panel.fade = None
        panel.shown = show
        duration = self._fade_ms()
        start = container.get_opacity() if container.get_visible() else 0.0
        target = 1.0 if show else 0.0

        def finish():
            container.set_opacity(1.0)
            container.set_can_target(True)
            container.set_visible(show)

        if duration <= 0 or start == target:
            finish()
            return
        if show:
            container.set_opacity(start)
            container.set_visible(True)
        else:
            # Still on screen while it fades, but no longer something to click.
            container.set_can_target(False)
        begin = {}

        def tick(widget, clock):
            now = clock.get_frame_time()
            begin.setdefault("at", now)
            progress = min(1.0, (now - begin["at"]) / (duration * 1000))
            eased = progress * progress * (3 - 2 * progress)
            widget.set_opacity(start + (target - start) * eased)
            if progress < 1.0:
                return GLib.SOURCE_CONTINUE
            panel.fade = None
            finish()
            return GLib.SOURCE_REMOVE

        panel.fade = container.add_tick_callback(tick)

    # -- following new messages ------------------------------------------

    def _scrolled(self, panel, adjustment):
        """The reader scrolled, or the log changed under the scrollbar. Only the reader's scrolling counts."""
        upper, page = adjustment.get_upper(), adjustment.get_page_size()
        if upper != panel.upper or page != panel.page:
            # A message grew or the panel was resized, which moves the scrollbar without the reader's doing.
            panel.upper, panel.page = upper, page
            return
        follow = upper - adjustment.get_value() - page <= FOLLOW_SLACK
        if DEBUG and follow != panel.follow:
            rsimgv.host.log(
                "debug",
                f"chat scroll: following {'on' if follow else 'off'} at {adjustment.get_value():.0f} "
                f"(content {upper:.0f}, window {page:.0f})",
            )
        panel.follow = follow
        if panel.to_bottom is not None:
            panel.to_bottom.set_visible(not follow)

    def _grew(self, panel, adjustment):
        panel.upper, panel.page = adjustment.get_upper(), adjustment.get_page_size()
        if panel.follow:
            adjustment.set_value(panel.upper - panel.page)

    def _scroll_down(self, panel):
        """Follows the log to its end, as when the reader has just sent a message or a reply has started."""
        panel.follow = True
        if panel.to_bottom is not None:
            panel.to_bottom.set_visible(False)
        self._follow_now(panel)
        self._keep_following(panel)

    def _watch(self, panel):
        """Measures the log again a few times a second while it is shown, so its scroll range cannot stay out of date."""
        if panel.key not in self._panels:
            return False
        if panel.in_viewer:
            overlay = panel.container.get_parent()
            if overlay is not None and panel.fitted != (overlay.get_width(), overlay.get_height()):
                self._fit_card(panel)
        if panel.scroll.get_mapped():
            panel.log.queue_resize()
            self._follow_now(panel)
        return True

    def _follow_now(self, panel):
        if panel.follow and panel.key in self._panels:
            adjustment = panel.scroll.get_vadjustment()
            end = adjustment.get_upper() - adjustment.get_page_size()
            if DEBUG and abs(adjustment.get_value() - end) > 0.5:
                rsimgv.host.log(
                    "debug",
                    f"chat scroll: moving {adjustment.get_value():.0f} to {end:.0f} "
                    f"(content {adjustment.get_upper():.0f}, window {adjustment.get_page_size():.0f})",
                )
            adjustment.set_value(end)

    def _keep_following(self, panel):
        """Pins the log to its end on every frame for a little while.

        A message is laid out over a frame or two after it changes, so the end it
        should scroll to is not known at once. Going to the end again on each
        frame, until the log has stopped changing, does not depend on when the
        scrollbar reports the new size. It runs while a reply is being written.
        """
        panel.settle = 12
        if panel.ticking:
            return
        panel.ticking = True

        def tick(widget, clock):
            if panel.key not in self._panels:
                panel.ticking = False
                return False
            # Measure the log again, so the end it scrolls to is its real one and not an earlier measurement.
            panel.log.queue_resize()
            self._follow_now(panel)
            if panel.streaming is not None:
                panel.settle = 12
            panel.settle -= 1
            if panel.settle <= 0:
                panel.ticking = False
                return False
            return True

        panel.scroll.add_tick_callback(tick)

    # -- sending ---------------------------------------------------------

    def _send_or_stop(self, panel):
        if self._request is not None:
            self._cancel_request()
        else:
            self._send_from(panel)

    def _send_from(self, panel):
        text = panel.entry.get_text().strip()
        if not text:
            return
        if self._send(text):
            panel.entry.set_text("")

    def _send(self, text):
        """Sends a message with the viewer's current context. Returns whether it was sent."""
        if self._request is not None:
            rsimgv.notify(f"{NAME}: a reply is still arriving.", seconds=2)
            return False
        if self._recorder is not None:
            rsimgv.notify(f"{NAME}: stop recording first.", seconds=2)
            return False

        self._stop_speech()
        command = quick_command(text) if self._config["quick_commands"] and self._config["tool_viewer"] else None
        if command is not None:
            self._run_quick(text, *command)
            return True
        context, filename, image, path = self._gather_context()
        message = {
            "id": uuid.uuid4().hex,
            "role": "user",
            "text": text,
            "file": filename,
            "context": context,
        }
        thumb = self._thumbnail(image, path)
        if path and thumb:
            message["path"] = path
            message["thumb"] = thumb
        self._history.append(message)
        self._trim_history()
        self._save_history()
        self._last_image = (message["id"], image)
        for panel in self._panels.values():
            self._add_bubble(panel, "user", text, filename, message=message)
            self._scroll_down(panel)
        self._begin_request(image)
        return True

    def _run_quick(self, text, name, args):
        """Does a short command such as "next image" at once, without asking the model."""
        result, note = self._execute_tool(Request(), name, args, checked=False)
        failed = result.startswith("Error")
        user = {"id": uuid.uuid4().hex, "role": "user", "text": text, "file": "", "context": ""}
        reply = {
            "id": uuid.uuid4().hex,
            "role": "assistant",
            "text": f"I could not do that: {result[7:]}" if failed else "Done.",
            "notes": [note],
        }
        self._history += [user, reply]
        self._trim_history()
        self._save_history()
        for panel in self._panels.values():
            self._add_bubble(panel, "user", text, "", message=user)
            self._add_bubble(panel, "assistant", reply["text"], None, message=reply)
            self._scroll_down(panel)
        self._refresh_controls()

    def _begin_request(self, image):
        """Starts a request for the conversation as it stands, which ends with a user message."""
        request = Request()
        self._request = request
        for panel in self._panels.values():
            panel.streaming = self._add_bubble(panel, "assistant", "", None, live=True)
            self._scroll_down(panel)
        self._refresh_controls()
        self._set_status("Waiting for the reply…")

        request.messages = self._build_messages(image)
        self._start_round(request)

    def _start_round(self, request):
        """Sends the conversation so far. A request is several rounds while the model uses tools."""
        request.calls = {}
        api_key = os.environ.get(self._config["api_key_env"], "").strip()
        body = self._request_body(request.messages, tools=request.rounds < self._config["tool_rounds"])
        request.sent_tools = "tools" in body
        self._debug(f"round {request.rounds + 1}: {len(body.get('tools', []))} tools sent, text mode {self._text_tools}")
        threading.Thread(
            target=self._chat_worker,
            args=(self._generation, request, api_key, body),
            daemon=True,
        ).start()

    def _regenerate(self):
        """Asks again for the last reply, or for one that failed, about the same message."""
        if self._request is not None:
            return
        if self._history and self._history[-1]["role"] == "assistant":
            self._history.pop()
            self._save_history()
        if not self._history or self._history[-1]["role"] != "user":
            return

        self._stop_speech()
        for panel in self._panels.values():
            if panel.bubbles and panel.bubbles[-1].role == "assistant":
                panel.log.remove(panel.bubbles.pop().box)
            self._scroll_down(panel)

        user = self._history[-1]
        image = None
        if self._last_image and self._last_image[0] == user.get("id"):
            image = self._last_image[1]
        elif self._config["send_image"]:
            # The picture was not kept, so use the one on screen if it is still the file asked about.
            _, filename, current, _ = self._gather_context()
            if filename == user.get("file"):
                image = current
        self._begin_request(image)

    def _gather_context(self):
        """The text, filename and screenshot for the item the viewer shows, on the main thread."""
        config = self._config
        try:
            info = rsimgv.host.call("view.info")
        except Exception:
            return "No picture or video is open in the viewer.", "", None, ""

        filename = info.get("filename") or ""
        directory = info.get("directory") or ""
        lines = []
        if config["send_metadata"]:
            lines.append(f"Filename: {filename}")
            lines.append(f"Folder: {directory}")
            if info.get("width") and info.get("height"):
                lines.append(f"Shown at: {info['width']}x{info['height']} px")
            if info.get("mime_type"):
                lines.append(f"Type: {info['mime_type']}")
            if info.get("duration") is not None:
                position = info.get("position") or 0
                lines.append(
                    f"Video position: {position:.2f} s of {info['duration']:.2f} s"
                    + (f", {info['fps']:.3g} fps" if info.get("fps") else "")
                )
            if filename and directory:
                try:
                    result = rsimgv.host.call(
                        "metadata.get", {"paths": [os.path.join(directory, filename)]}
                    )
                    entries = result.get("metadata") or []
                    if entries:
                        lines.append("File metadata: " + json.dumps(entries[0], ensure_ascii=False))
                except Exception as exc:
                    lines.append(f"File metadata: unavailable ({exc})")
        elif filename:
            lines.append(f"Filename: {filename}")

        image = None
        if config["send_image"]:
            try:
                data = rsimgv.host.call("view.screenshot").get("data")
            except Exception as exc:
                lines.append(f"The picture could not be captured: {exc}")
                data = None
            if isinstance(data, str) and data:
                jpeg = scaled_jpeg(data, SENT_IMAGE_SIZE, SENT_IMAGE_QUALITY)
                if jpeg is None:
                    lines.append("The picture could not be converted to send.")
                elif len(jpeg) > MAX_BASE64:
                    lines.append("The picture is too large to send.")
                else:
                    # Kept as a complete data URL, so everything that sends it names its format correctly.
                    image = f"data:image/jpeg;base64,{jpeg}"
        return "\n".join(lines), filename, image, os.path.join(directory, filename) if filename else ""

    def _build_messages(self, image):
        """The chat completions messages: the system prompt, recent history, and the picture with the last message."""
        recent = self._history[-self._config["history_messages"]:]
        while recent and recent[0]["role"] != "user":
            recent = recent[1:]

        messages = []
        system = self._config["system_prompt"].strip()
        if self._listener is not None:
            # One system message, since not every server accepts two.
            system = f"{system}\n\n{self._config['voice_prompt'].strip()}".strip()
        if self._config["memory"]:
            system = f"{system}\n\n{self._memory_prompt()}".strip()
        if self._text_tools:
            system = f"{system}\n\n{self._prompted_tools()}".strip()
        if self._config["tools"] and self._config["tool_viewer"]:
            system = (
                f"{system}\n\nYou can control the viewer with tools: move between items, zoom, pan, rotate, play, "
                "pause, seek, and change speed and volume. Use them when the user asks, and then say briefly what "
                "you did. You do not see zoom or pan; after changing the item or a video's position, use look to "
                "see the result."
            ).strip()
        if system:
            messages.append({"role": "system", "content": system})

        last = len(recent) - 1
        for index, message in enumerate(recent):
            if message["role"] == "assistant":
                messages.append({"role": "assistant", "content": message["text"]})
                continue
            if index == last:
                text = message["text"]
                if message.get("context"):
                    text = f"[Viewer]\n{message['context']}\n[End of viewer]\n\n{text}"
                if image:
                    content = [
                        {"type": "text", "text": text},
                        {"type": "image_url", "image_url": {"url": image}},
                    ]
                else:
                    content = text
            else:
                # Earlier pictures and metadata are not resent, only which file was shown.
                filename = message.get("file")
                content = f"[Viewing {filename}]\n{message['text']}" if filename else message["text"]
            messages.append({"role": message["role"], "content": content})
        return messages

    def _cancel_request(self):
        request = self._request
        if request is None:
            return
        request.cancel.set()
        self._request = None
        request.finish()
        thinking, reply = request.texts()
        reply = strip_memory_tags(reply)
        message = self._remember_reply(thinking, reply, request.notes) if reply.strip() else None
        self._finish_bubbles(
            thinking,
            reply + ("\n\n(stopped)" if reply else "(stopped)"),
            message_id=message["id"] if message else None,
        )
        self._set_status("")
        self._refresh_controls()

    def _request_body(self, messages, tools=True):
        """The chat completions request: the settings' parameters, then the thinking choice, then the messages."""
        config = self._config
        body = {"model": config["model"], "max_tokens": config["max_tokens"]}
        if self._temperature is not None:
            body["temperature"] = self._temperature
        if self._top_p is not None:
            body["top_p"] = self._top_p
        deep_merge(body, self._extra_params)
        deep_merge(body, self._thinking_on if self._thinking else self._thinking_off)
        # These two are the plugin's own, so no parameter can change them.
        body["messages"] = messages
        if config["stream"]:
            body["stream"] = True
        else:
            body.pop("stream", None)
        # The last round has no tools, so the model has to answer.
        specs = self._tool_specs() if tools and not self._text_tools else []
        if specs:
            body["tools"] = specs
        return body

    def _chat_worker(self, generation, request, api_key, body):
        config = self._config
        try:
            response = self._open(
                f"{config['base_url'].rstrip('/')}/chat/completions",
                json.dumps(body).encode("utf-8"),
                {"Content-Type": "application/json"},
                api_key,
            )
            with response:
                if config["stream"]:
                    self._read_stream(generation, request, response)
                else:
                    self._read_reply(request, json.loads(response.read().decode("utf-8", "replace")))
                    self._post_partial(generation, request)
            error = None
        except Exception as exc:
            error = str(exc)

        request.finish()
        rsimgv.ui.idle_add(lambda: self._round_done(generation, request, error))

    def _read_stream(self, generation, request, response):
        for raw in response:
            if request.cancel.is_set():
                return
            line = raw.decode("utf-8", "replace").strip()
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if payload == "[DONE]":
                return
            try:
                chunk = json.loads(payload)
            except ValueError:
                continue
            if not isinstance(chunk, dict):
                continue
            if chunk.get("error"):
                raise RuntimeError(self._error_message(chunk["error"]))
            changed = False
            for choice in chunk.get("choices") or []:
                delta = choice.get("delta") or {}
                changed |= self._take_reasoning(request, delta)
                piece = delta.get("content")
                if isinstance(piece, str) and piece:
                    request.feed(piece)
                    changed = True
                self._take_tool_calls(request, delta)
            if changed:
                self._post_partial(generation, request)

    @staticmethod
    def _take_tool_calls(request, message):
        """Collects the tool calls a server reports, which a stream sends in pieces."""
        for position, call in enumerate(message.get("tool_calls") or []):
            if not isinstance(call, dict):
                continue
            entry = request.calls.setdefault(call.get("index", position), {"id": "", "name": "", "arguments": ""})
            if call.get("id"):
                entry["id"] = call["id"]
            function = call.get("function") or {}
            if function.get("name") and not entry["name"]:
                entry["name"] = function["name"]
            arguments = function.get("arguments")
            if isinstance(arguments, dict):
                entry["arguments"] = json.dumps(arguments)
            elif isinstance(arguments, str):
                entry["arguments"] += arguments

    @staticmethod
    def _take_reasoning(request, message):
        """Takes the reasoning a server reports apart from the reply. Returns whether there was any."""
        for key in ("reasoning_content", "reasoning"):
            piece = message.get(key)
            if isinstance(piece, str) and piece:
                request.add(True, piece)
                return True
        return False

    def _read_reply(self, request, value):
        """Takes the reply and reasoning out of a response that was not streamed."""
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(self._error_message(value["error"]))
        choices = value.get("choices") or []
        if not choices:
            raise RuntimeError("the response has no choices")
        message = choices[0].get("message", {})
        self._take_reasoning(request, message)
        self._take_tool_calls(request, message)
        content = message.get("content")
        if isinstance(content, list):
            content = "\n".join(
                part.get("text", "") for part in content if isinstance(part, dict) and part.get("text")
            )
        if content is None and (request.thinking or request.calls):
            return
        if not isinstance(content, str):
            raise RuntimeError("the response has no text")
        request.feed(content)

    def _post_partial(self, generation, request):
        thinking, reply = request.texts()
        rsimgv.ui.idle_add(lambda: self._partial(generation, request, thinking, reply))

    def _partial(self, generation, request, thinking, reply):
        if not self._current(generation) or self._request is not request:
            return
        reply = strip_memory_tags(reply)
        first = not request.arrived
        request.arrived = True
        for panel in self._panels.values():
            if panel.streaming is not None:
                panel.streaming.show(thinking, reply)
                # The first of a response always brings the log to its end; after that it follows only a reader who is there.
                if first:
                    self._scroll_down(panel)
                else:
                    self._keep_following(panel)
        self._set_status("" if reply else ("Thinking…" if thinking else "Waiting for the reply…"))

    def _round_done(self, generation, request, error):
        """A round has been read: run the tools it asked for and go on, or finish if it answered."""
        if not self._current(generation) or self._request is not request:
            return
        calls = [request.calls[index] for index in sorted(request.calls) if request.calls[index]["name"]]
        round_text = "".join(request.reply[request.round_start:])
        self._debug(f"round read: {len(calls)} structured calls, error {error!r}, reply {round_text[:80]!r}")

        # A server that does not structure the model's tool calls leaves them in its reply as text.
        if not calls and error is None and self._tool_specs():
            written, cleaned = parse_text_tool_calls(round_text)
            if written:
                self._debug(f"{len(written)} tool calls written as text")
                del request.reply[request.round_start:]
                request.reply.append(cleaned)
                request.round_raw = round_text
                calls = [
                    {"id": "", "name": name, "arguments": arguments if isinstance(arguments, str) else json.dumps(arguments)}
                    for name, arguments in written
                ]

        # A server that rejects the tools field gets them described in the prompt instead.
        if (
            error is not None
            and request.sent_tools
            and not self._text_tools
            and not round_text.strip()
            and re.search(r"tool|jinja|function", error, flags=re.I)
        ):
            self._text_tools = True
            self._debug(f"the server rejected tools ({error}); asking for them in the prompt")
            rsimgv.notify(
                f"{NAME}: the server does not accept tools, so they are described in the prompt instead.",
                level="warning",
                seconds=6,
            )
            if request.messages and request.messages[0]["role"] == "system":
                request.messages[0]["content"] += f"\n\n{self._prompted_tools()}"
            else:
                request.messages.insert(0, {"role": "system", "content": self._prompted_tools()})
            self._start_round(request)
            return

        if calls and error is None:
            self._run_tools(request, calls)
        else:
            self._chat_done(generation, request, error)

    def _prompted_tools(self):
        return PROMPTED_TOOLS + describe_tools(self._tool_specs())

    def _run_tools(self, request, calls):
        """Runs a round's tool calls, adds them and their results to the conversation, and asks again."""
        round_text = strip_memory_tags("".join(request.reply[request.round_start:]))
        for call in calls:
            call["id"] = call["id"] or f"call_{uuid.uuid4().hex[:12]}"
        if self._text_tools:
            # With no tools field the exchange is plain text both ways, as the model wrote the calls.
            written = request.round_raw or "\n".join(
                f'<tool_call>{{"name": "{call["name"]}", "arguments": {call["arguments"] or "{}"}}}</tool_call>'
                for call in calls
            )
            request.messages.append({"role": "assistant", "content": written})
        else:
            request.messages.append(
                {
                    "role": "assistant",
                    "content": round_text,
                    "tool_calls": [
                        {
                            "id": call["id"],
                            "type": "function",
                            "function": {"name": call["name"], "arguments": call["arguments"] or "{}"},
                        }
                        for call in calls
                    ],
                }
            )
        request.round_raw = ""
        results = []

        self._set_status("Using tools…")
        for call in calls:
            try:
                arguments = json.loads(call["arguments"] or "{}")
                if not isinstance(arguments, dict):
                    raise ValueError("not an object")
            except ValueError:
                result, note = "Error: the arguments were not a valid JSON object.", f"{call['name']}: bad arguments"
            else:
                result, note = self._execute_tool(request, call["name"], arguments)
            self._debug(f"tool {call['name']} {call['arguments']!r} -> {result[:100]!r}")
            results.append(f"[{call['name']}] {result}")
            if not self._text_tools:
                request.messages.append({"role": "tool", "tool_call_id": call["id"], "content": result})
            request.notes.append(note)
            for panel in self._panels.values():
                if panel.streaming is not None:
                    panel.streaming.add_note(note)

        if self._text_tools:
            request.messages.append({"role": "user", "content": "Tool results:\n" + "\n".join(results)})
        if request.look is not None:
            details, image = request.look
            request.look = None
            request.messages.append(
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": f"[Viewer]\n{details}\n[End of viewer]\nThis is what the viewer shows now."},
                        {"type": "image_url", "image_url": {"url": image}},
                    ],
                }
            )

        request.rounds += 1
        if request.reply:
            request.reply.append("\n\n")
        request.round_start = len(request.reply)
        self._set_status("Waiting for the reply…")
        self._start_round(request)

    def _chat_done(self, generation, request, error):
        if not self._current(generation) or self._request is not request:
            return
        self._request = None
        thinking, raw = request.texts()
        thinking, raw = thinking.strip(), raw.strip()
        reply = strip_memory_tags(raw)
        acted = self._apply_memory_actions(raw) or bool(request.notes)

        if error is not None and not reply and not acted:
            self._finish_bubbles(thinking, error, failed=True)
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=10)
        elif not reply and not acted:
            self._finish_bubbles(thinking, "(the model returned an empty reply)", failed=True)
        else:
            # A reply that only used tools or saved a memory has nothing to say, so nothing is spoken.
            speakable = bool(reply)
            reply = reply or ("(done)" if request.notes else "(memory updated)")
            message = self._remember_reply(thinking, reply, request.notes)
            self._finish_bubbles(thinking, reply, message_id=message["id"])
            if error is not None:
                rsimgv.notify(f"{NAME}: the reply was cut short: {error}", level="warning", seconds=8)
            elif speakable and (self._speak or self._listener is not None) and self._config["tts_backend"] != "off":
                self._speak_text(reply)

        self._set_status("")
        self._refresh_controls()

    def _remember_reply(self, thinking, reply, notes=None):
        message = {"id": uuid.uuid4().hex, "role": "assistant", "text": reply.strip()}
        if thinking.strip():
            message["thinking"] = thinking.strip()
        if notes:
            message["notes"] = list(notes)
        self._history.append(message)
        self._trim_history()
        self._save_history()
        return message

    def _finish_bubbles(self, thinking, text, failed=False, message_id=None):
        for panel in self._panels.values():
            bubble = panel.streaming
            panel.streaming = None
            if bubble is not None:
                bubble.message_id = message_id
                bubble.show(thinking, text, final=True, failed=failed)
                self._keep_following(panel)

    # -- tools -----------------------------------------------------------

    def _tool_specs(self):
        """The tools sent to the model: memory ones when memory is on, viewer ones when the viewer may be controlled."""
        config = self._config
        specs = []
        if config["tools"]:
            if config["memory"]:
                specs += MEMORY_TOOLS
            if config["tool_viewer"]:
                specs += VIEWER_TOOLS
        return specs

    def _debug(self, text):
        if DEBUG:
            rsimgv.host.log("debug", f"chat tools: {text}")

    def _execute_tool(self, request, name, args, checked=True):
        """Runs one tool call on the main thread. Returns (what the model is told, a note for the chat).

        A tool that is not enabled is refused, unless `checked` is off, as for a command the user typed.
        """
        available = [spec["function"]["name"] for spec in self._tool_specs()]
        if checked and name not in available:
            return (
                f"Error: there is no tool called {name}. The tools are: {', '.join(available)}.",
                f"Unknown tool: {name}",
            )
        try:
            return getattr(self, f"_tool_{name}")(request, args)
        except Exception as exc:
            return f"Error: {exc}", f"{name} failed: {exc}"

    @staticmethod
    def _number(args, key):
        try:
            return float(args[key])
        except (KeyError, TypeError, ValueError) as exc:
            raise ValueError(f"{key} must be a number") from exc

    @staticmethod
    def _steps(args):
        try:
            return max(1, min(10, int(args.get("steps", 1))))
        except (TypeError, ValueError):
            return 1

    @staticmethod
    def _action(name, value=None):
        """Runs one of the application's actions, as a keybinding would."""
        params = {"name": name}
        if value is not None:
            params["value"] = value
        try:
            rsimgv.host.call("actions.run", params)
        except Exception as exc:
            raise RuntimeError(f"{exc}. The viewer may not be showing a suitable item.") from exc

    def _memory_at(self, args):
        number = int(self._number(args, "number"))
        if not 1 <= number <= len(self._memories):
            raise ValueError(f"there is no memory {number}; there are {len(self._memories)}")
        return number, self._memories[number - 1]

    def _tool_remember(self, request, args):
        text = " ".join(str(args.get("text", "")).split())
        if not text:
            raise ValueError("text is empty")
        if self._remember(text):
            return f"Saved as memory {len(self._memories)}.", f"Remembered: {text}"
        return "That is already remembered.", f"Already remembered: {text}"

    def _tool_forget(self, request, args):
        number, memory = self._memory_at(args)
        self._forget(memory["id"])
        return f"Forgot memory {number}: {memory['text']}", f"Forgot: {memory['text']}"

    def _tool_update_memory(self, request, args):
        number, memory = self._memory_at(args)
        text = " ".join(str(args.get("text", "")).split())
        if not text:
            raise ValueError("text is empty")
        self._update_memory(memory["id"], text)
        return f"Memory {number} now says: {text}", f"Updated memory: {text}"

    def _tool_list_memories(self, request, args):
        if not self._memories:
            return "No memories are saved.", "Listed the memories"
        listing = "\n".join(f"[{number}] {memory['text']}" for number, memory in enumerate(self._memories, 1))
        return listing, "Listed the memories"

    def _tool_look(self, request, args):
        context, filename, image, _ = self._gather_context()
        if image:
            request.look = (context, image)
        result = context + ("\nThe picture is attached in the next message." if image else "")
        return result.strip() or "Nothing is shown.", f"Looked at {filename}" if filename else "Looked at the viewer"

    def _tool_navigate(self, request, args):
        direction = args.get("to")
        if direction not in NAVIGATE_ACTIONS:
            raise ValueError("to must be next, previous, first or last")
        try:
            self._action(NAVIGATE_ACTIONS[direction])
        except Exception:
            # The viewer is not open, so move the selection in the folder being browsed instead.
            current = rsimgv.host.call("files.selected").get("index")
            index = {
                "next": 0 if current is None else current + 1,
                "previous": -1 if current is None else current - 1,
                "first": 0,
                "last": -1,
            }[direction]
            try:
                selected = rsimgv.host.call("files.select_index", {"index": index}).get("selected")
            except Exception as exc:
                raise RuntimeError(f"there is no {direction} item here") from exc
            return (
                f"The viewer is not open, so the selection in the folder moved to the {direction} item: {selected}.",
                f"{direction.capitalize()} item",
            )
        return f"Moved to the {direction} item. Use look to see it.", f"{direction.capitalize()} item"

    def _tool_zoom(self, request, args):
        choice = args.get("action")
        if choice not in ZOOM_ACTIONS:
            raise ValueError(f"action must be one of {', '.join(ZOOM_ACTIONS)}")
        steps = self._steps(args) if choice in ("in", "out") else 1
        for _ in range(steps):
            self._action(ZOOM_ACTIONS[choice])
        note = ZOOM_LABELS[choice] + (f" ×{steps}" if steps > 1 else "")
        return f"Done: {note.lower()}.", note

    def _tool_pan(self, request, args):
        direction = args.get("direction")
        if direction not in ("up", "down", "left", "right"):
            raise ValueError("direction must be up, down, left or right")
        steps = self._steps(args)
        for _ in range(steps):
            self._action(f"image.pan-{direction}")
        note = f"Pan {direction}" + (f" ×{steps}" if steps > 1 else "")
        return f"Done: {note.lower()}.", note

    def _tool_rotate(self, request, args):
        degrees = int(self._number(args, "degrees"))
        if degrees not in (0, 90, 180, 270):
            raise ValueError("degrees must be 0, 90, 180 or 270")
        self._action("view.set-rotation", degrees)
        return f"Rotated to {degrees} degrees.", f"Rotate to {degrees}°"

    def _tool_flip(self, request, args):
        direction = args.get("direction")
        if direction not in ("horizontal", "vertical"):
            raise ValueError("direction must be horizontal or vertical")
        self._action(f"view.flip-{direction}")
        return f"Flipped {direction}ly.", f"Flip {direction}"

    def _tool_playback(self, request, args):
        try:
            position = rsimgv.host.call("view.info").get("position")
        except Exception:
            position = None
        self._action("video.play-pause")
        where = f" (the video was at {position:.1f} s)" if isinstance(position, (int, float)) else ""
        return f"Toggled playback{where}. Use look to check whether it is playing.", "Play or pause"

    def _tool_seek(self, request, args):
        if args.get("frames") is not None:
            frames = int(self._number(args, "frames"))
            self._action("video.frame-seek", frames)
            return f"Stepped {frames:+d} frames.", f"Step {frames:+d} frames"
        seconds = self._number(args, "seconds")
        self._action("video.seek", seconds)
        return f"Moved {seconds:+g} seconds. Use look to see the frame.", f"Seek {seconds:+g} s"

    def _tool_speed(self, request, args):
        speed = self._number(args, "speed")
        self._action("video.set-speed", speed)
        return f"Playback speed is now {speed:g}.", f"Speed ×{speed:g}"

    def _tool_volume(self, request, args):
        done, notes = [], []
        if args.get("change_percent") is not None:
            change = self._number(args, "change_percent")
            self._action("video.volume", change)
            done.append(f"changed the volume by {change:+g}%")
            notes.append(f"Volume {change:+g}%")
        if args.get("toggle_mute"):
            self._action("video.mute")
            done.append("toggled mute")
            notes.append("Mute or unmute")
        if not done:
            raise ValueError("give change_percent, or toggle_mute")
        return f"Done: {' and '.join(done)}.", ", ".join(notes)

    def _tool_fullscreen(self, request, args):
        self._action("win.toggle-fullscreen")
        return "Toggled fullscreen.", "Fullscreen"

    # -- memory ----------------------------------------------------------

    def _memory_prompt(self):
        """What the model is told about its memory: what is saved, and how to save or forget something."""
        self._memory_ids = [memory["id"] for memory in self._memories]
        if self._memories:
            saved = "Saved memories:\n" + "\n".join(
                f"[{number}] {memory['text']}" for number, memory in enumerate(self._memories, 1)
            )
        else:
            saved = "No memories are saved yet."
        if self._config["tools"]:
            return (
                "You have a persistent memory that is shared by every conversation. "
                f"{saved}\n"
                "When the user tells you something worth keeping for later, save it with the remember tool; "
                "change or delete a memory with update_memory or forget, by its number. Save sparingly, and never "
                "save what is already saved."
            )
        return (
            "You have a persistent memory that is shared by every conversation. "
            f"{saved}\n"
            "When the user tells you something worth keeping for later, such as their name, preferences, people, "
            "projects, or anything they ask you to remember, save it by writing <memory>one short, complete "
            "fact</memory> in your reply. To forget a saved memory, write <forget>its number</forget>. Save "
            "sparingly, never save what is already saved, and never mention or explain these tags."
        )

    def _apply_memory_actions(self, raw):
        """Saves and forgets what a reply asked for. Returns whether anything changed."""
        if not self._config["memory"]:
            return False
        changed = False
        for match in re.finditer(r"<memory>(.*?)</memory>", raw, flags=re.S):
            changed |= self._remember(match.group(1), announce=True)
        for match in re.finditer(r"<forget>\s*(\d+)\s*</forget>", raw):
            number = int(match.group(1))
            if 1 <= number <= len(self._memory_ids):
                changed |= self._forget(self._memory_ids[number - 1], announce=True)
        return changed

    def _remember(self, text, announce=False):
        """Saves a memory. Returns False for an empty or repeated one."""
        text = " ".join(text.split())[:500]
        if not text or any(memory["text"].lower() == text.lower() for memory in self._memories):
            return False
        self._memories.append({"id": uuid.uuid4().hex, "text": text, "created": int(time.time())})
        # The oldest are dropped when there are too many.
        self._memories = self._memories[-self._config["memory_limit"]:]
        self._save_memories()
        self._refresh_memory()
        if announce:
            rsimgv.notify(f"Remembered: {text}", seconds=3)
        return True

    def _forget(self, memory_id, announce=False):
        for memory in self._memories:
            if memory["id"] == memory_id:
                self._memories.remove(memory)
                self._save_memories()
                self._refresh_memory()
                if announce:
                    rsimgv.notify(f"Forgot: {memory['text']}", seconds=3)
                return True
        return False

    def _update_memory(self, memory_id, text):
        for memory in self._memories:
            if memory["id"] == memory_id:
                memory["text"] = text[:500]
                self._save_memories()
                self._refresh_memory()
                return True
        return False

    def _add_memory_from(self, panel):
        if self._remember(panel.memory_entry.get_text()):
            panel.memory_entry.set_text("")

    def _refresh_memory(self):
        """Lists the memories in each panel's popover, each with a button to forget it."""
        for panel in self._panels.values():
            child = panel.memory_list.get_first_child()
            while child is not None:
                following = child.get_next_sibling()
                panel.memory_list.remove(child)
                child = following
            if not self._memories:
                empty = Gtk.Label(label="Nothing is remembered yet.", xalign=0.0)
                empty.add_css_class("dim-label")
                panel.memory_list.append(empty)
            for memory in self._memories:
                label = Gtk.Label(label=memory["text"], xalign=0.0, hexpand=True)
                label.set_wrap(True)
                label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
                label.set_max_width_chars(36)
                forget = Gtk.Button.new_from_icon_name("user-trash-symbolic")
                forget.add_css_class("flat")
                forget.add_css_class("circular")
                forget.set_valign(Gtk.Align.START)
                forget.set_tooltip_text("Forget this")
                forget.connect("clicked", lambda *_, memory_id=memory["id"]: self._forget(memory_id))
                row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=4)
                row.append(label)
                row.append(forget)
                panel.memory_list.append(row)

    def _load_memories(self):
        try:
            with open(self._memory_path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
        except FileNotFoundError:
            return []
        except Exception as exc:
            rsimgv.notify(f"{NAME}: cannot read {self._memory_path}: {exc}", level="warning", seconds=8)
            return []
        return [
            {"id": str(item.get("id") or uuid.uuid4().hex), "text": item["text"], "created": item.get("created", 0)}
            for item in (loaded if isinstance(loaded, list) else [])
            if isinstance(item, dict) and isinstance(item.get("text"), str) and item["text"].strip()
        ]

    def _save_memories(self):
        temporary = f"{self._memory_path}.tmp"
        try:
            os.makedirs(os.path.dirname(self._memory_path), exist_ok=True)
            with open(temporary, "w", encoding="utf-8") as file:
                json.dump(self._memories, file, ensure_ascii=False, indent=2)
            os.replace(temporary, self._memory_path)
        except OSError as exc:
            rsimgv.notify(f"{NAME}: cannot save the memories: {exc}", level="warning", seconds=8)

    # -- messages --------------------------------------------------------

    def _thumbnail(self, image, path):
        """A small picture to keep beside a message: the frame that was sent, or else the file's own thumbnail."""
        thumb = make_thumbnail(image.split(",", 1)[1]) if image else None
        if thumb or not path:
            return thumb
        try:
            result = rsimgv.host.call("thumbnails.get", {"paths": [path], "max_size": THUMB_SIZE * 2})
            data = ((result.get("thumbnails") or [{}])[0]).get("data")
        except Exception:
            return None
        return make_thumbnail(data) if data else None

    def _delete(self, bubble):
        """Deletes a message. Deleting one you sent deletes the reply to it too, since the reply has no question."""
        if self._request is not None:
            return
        doomed = set()
        if bubble.message_id:
            for index, message in enumerate(self._history):
                if message["id"] != bubble.message_id:
                    continue
                doomed.add(message["id"])
                following = self._history[index + 1] if index + 1 < len(self._history) else None
                if message["role"] == "user" and following and following["role"] == "assistant":
                    doomed.add(following["id"])
            self._history = [message for message in self._history if message["id"] not in doomed]
            self._save_history()

        for panel in self._panels.values():
            for other in list(panel.bubbles):
                if other is bubble or (other.message_id and other.message_id in doomed):
                    panel.log.remove(other.box)
                    panel.bubbles.remove(other)
        self._refresh_controls()

    def _open_in_viewer(self, path):
        """Shows a picture or video in the viewer, going to its folder first if it is elsewhere."""
        if not os.path.isfile(path):
            rsimgv.notify(f"{NAME}: {os.path.basename(path)} is no longer there.", level="warning", seconds=5)
            return
        folder, name = os.path.split(path)
        try:
            info = rsimgv.host.call("view.info")
        except Exception:
            info = None
        if info and info.get("directory") == folder and info.get("filename") == name:
            return
        try:
            if info:
                # Back to the browse view, so the folder can change under it.
                rsimgv.host.call("actions.run", {"name": "win.toggle-viewer"})
            if rsimgv.host.call("window.info").get("current_path") != folder:
                rsimgv.host.call("navigate.to", {"path": folder})
        except Exception as exc:
            rsimgv.notify(f"{NAME}: cannot open {name}: {exc}", level="error", seconds=6)
            return
        self._select_and_open(self._generation, folder, name, 40)

    def _select_and_open(self, generation, folder, name, attempts):
        """Selects the file once its folder has loaded, then shows it. Retries a while, since folders load in the background."""
        if not self._current(generation):
            return
        try:
            loaded = rsimgv.host.call("window.info").get("current_path") == folder
            if loaded:
                rsimgv.host.call("files.select", {"file": name})
                rsimgv.host.call("actions.run", {"name": "win.toggle-viewer"})
                return
        except Exception:
            pass
        if attempts > 0:
            rsimgv.ui.idle_add(lambda: self._select_and_open(generation, folder, name, attempts - 1))
        else:
            rsimgv.notify(f"{NAME}: cannot find {name} in its folder.", level="warning", seconds=6)

    # -- replaying -------------------------------------------------------

    def _replay(self, bubble):
        if bubble.failed or not bubble.text.strip():
            return
        if self._config["tts_backend"] == "off":
            rsimgv.notify(f"{NAME}: choose a speech backend in the plugin settings.", seconds=4)
            return
        self._stop_speech()
        self._speak_text(bubble.text)

    # -- voice input -----------------------------------------------------

    def _toggle_record(self, panel):
        if self._recorder is not None:
            self._stop_recording()
        else:
            self._start_recording(panel)

    def _start_recording(self, panel):
        if self._request is not None:
            rsimgv.notify(f"{NAME}: wait for the reply to finish.", seconds=2)
            return
        if self._transcribing:
            rsimgv.notify(f"{NAME}: still transcribing.", seconds=2)
            return
        if self._listener is not None:
            rsimgv.notify(f"{NAME}: a voice conversation is already listening.", seconds=2)
            return

        # Speaking over the microphone would be recorded, so silence it first.
        self._stop_speech()
        directory = tempfile.mkdtemp(prefix="rsimgv-chat-")
        output = os.path.join(directory, "speech.wav")
        try:
            args = self._command(self._config["record_command"], output=output)
            recorder = subprocess.Popen(
                args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
            )
        except Exception as exc:
            shutil.rmtree(directory, ignore_errors=True)
            rsimgv.notify(f"{NAME}: cannot record: {exc}", level="error", seconds=8)
            return

        self._recorder = recorder
        self._record_dir = directory
        self._record_panel = panel.key
        self._set_status("Recording… press the microphone again to send")
        self._refresh_controls()

    def _stop_recording(self):
        recorder, directory, key = self._recorder, self._record_dir, self._record_panel
        self._recorder = self._record_dir = self._record_panel = None
        self._transcribing = True
        self._set_status("Transcribing…")
        self._refresh_controls()
        threading.Thread(
            target=self._transcribe_worker,
            args=(self._generation, recorder, directory, key),
            daemon=True,
        ).start()

    def _abandon_recording(self):
        recorder, directory = self._recorder, self._record_dir
        self._recorder = self._record_dir = self._record_panel = None
        if recorder is not None and recorder.poll() is None:
            recorder.terminate()
        if directory:
            shutil.rmtree(directory, ignore_errors=True)

    def _transcribe_worker(self, generation, recorder, directory, key):
        text, error = "", None
        try:
            # SIGINT lets the recorder finish the WAV header before it exits.
            if recorder.poll() is None:
                recorder.send_signal(signal.SIGINT)
            try:
                recorder.wait(timeout=5)
            except subprocess.TimeoutExpired:
                recorder.kill()
                recorder.wait()

            path = os.path.join(directory, "speech.wav")
            if not os.path.isfile(path) or os.path.getsize(path) <= 44:
                detail = (recorder.stderr.read() or b"").decode("utf-8", "replace").strip()
                raise RuntimeError(f"nothing was recorded. {detail[-200:]}".strip())
            text = self._transcribe(path)
            if not text:
                raise RuntimeError("no speech was recognised")
        except Exception as exc:
            error = str(exc)
        finally:
            shutil.rmtree(directory, ignore_errors=True)

        rsimgv.ui.idle_add(lambda: self._transcribed(generation, key, text, error))

    def _transcribe(self, path):
        config = self._config
        if config["asr_backend"] == "command":
            args = self._command(config["asr_command"], input=path)
            try:
                result = subprocess.run(
                    args,
                    stdin=subprocess.DEVNULL,
                    capture_output=True,
                    timeout=config["timeout_seconds"],
                )
            except subprocess.TimeoutExpired as exc:
                raise RuntimeError("the transcription command timed out") from exc
            if result.returncode != 0:
                detail = result.stderr.decode("utf-8", "replace").strip()
                raise RuntimeError(f"{args[0]} failed: {detail[-300:]}")
            lines = result.stdout.decode("utf-8", "replace").splitlines()
            return " ".join(line.strip() for line in lines if line.strip())

        base = (config["asr_url"] or config["base_url"]).rstrip("/")
        boundary = uuid.uuid4().hex
        with open(path, "rb") as file:
            audio = file.read()
        body = b"".join(
            [
                f'--{boundary}\r\nContent-Disposition: form-data; name="model"\r\n\r\n'
                f"{config['asr_model']}\r\n".encode("utf-8"),
                f'--{boundary}\r\nContent-Disposition: form-data; name="file"; '
                f'filename="speech.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode("utf-8"),
                audio,
                f"\r\n--{boundary}--\r\n".encode("utf-8"),
            ]
        )
        api_key = os.environ.get(config["api_key_env"], "").strip()
        with self._open(
            f"{base}/audio/transcriptions",
            body,
            {"Content-Type": f"multipart/form-data; boundary={boundary}"},
            api_key,
        ) as response:
            value = json.loads(response.read().decode("utf-8", "replace"))
        text = value.get("text") if isinstance(value, dict) else None
        return text.strip() if isinstance(text, str) else ""

    def _transcribed(self, generation, key, text, error):
        if not self._current(generation):
            return
        self._transcribing = False
        self._set_status("")
        self._refresh_controls()
        if error is not None:
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=8)
            return

        panel = self._panels.get(key) or next(iter(self._panels.values()), None)
        if self._config["auto_send_voice"]:
            self._send(text)
        elif panel is not None:
            panel.entry.set_text(text)
            panel.entry.grab_focus()

    # -- request parameters ----------------------------------------------

    def _parse_parameters(self):
        """Reads the request parameters from the settings, telling the user about any that cannot be used."""
        config = self._config
        self._thinking = config["thinking"]
        self._temperature = self._parse_number("Temperature", config["temperature"])
        self._top_p = self._parse_number("Top p", config["top_p"])
        self._extra_params = self._parse_object("Extra request parameters", config["extra_params"])
        self._thinking_on = self._parse_object("Parameters for thinking on", config["thinking_on_params"])
        self._thinking_off = self._parse_object("Parameters for thinking off", config["thinking_off_params"])

    @staticmethod
    def _parse_number(name, text):
        text = text.strip()
        if not text:
            return None
        try:
            return float(text)
        except ValueError:
            rsimgv.notify(f"{NAME}: {name} must be a number, so it is not sent.", level="warning", seconds=8)
            return None

    @staticmethod
    def _parse_object(name, text):
        text = text.strip()
        if not text:
            return {}
        try:
            value = json.loads(text)
        except ValueError as exc:
            rsimgv.notify(f"{NAME}: {name} is not valid JSON ({exc}), so it is not sent.", level="warning", seconds=10)
            return {}
        if not isinstance(value, dict):
            rsimgv.notify(f"{NAME}: {name} must be a JSON object, so it is not sent.", level="warning", seconds=10)
            return {}
        return value

    def _toggle_thinking(self):
        self._thinking = not self._thinking
        self._save_setting("thinking", self._thinking)
        self._refresh_controls()

    def _save_setting(self, key, value):
        """Keeps a setting changed in the panel, by merging it into the plugin's config file."""
        try:
            try:
                with open(self._config_file, "r", encoding="utf-8") as file:
                    saved = json.load(file)
            except FileNotFoundError:
                saved = {}
            if not isinstance(saved, dict):
                saved = {}
            saved[key] = value
            temporary = f"{self._config_file}.tmp"
            os.makedirs(os.path.dirname(self._config_file), exist_ok=True)
            with open(temporary, "w", encoding="utf-8") as file:
                json.dump(saved, file, indent=2)
            os.replace(temporary, self._config_file)
        except (OSError, ValueError) as exc:
            rsimgv.notify(f"{NAME}: cannot save the setting: {exc}", level="warning", seconds=8)

    # -- styling ----------------------------------------------------------

    def _ensure_style(self):
        """Adds the plugin's CSS for the display: the speech bubbles, and the viewer card's opacity.

        Only the card's own background is made see-through, not its text, so a
        rule for one class is used rather than fading the whole widget.
        """
        if self._card_style is not None:
            return
        display = Gdk.Display.get_default()
        if display is None:
            return
        opacity = self._config["card_opacity"] / 100
        provider = Gtk.CssProvider()
        provider.load_from_string(
            f"""
            .llm-chat-card {{ background-color: alpha(@window_bg_color, {opacity:.2f}); }}
            .llm-bubble {{ border-radius: 14px; padding: 6px 10px; }}
            .llm-bubble-user {{ background-color: @accent_bg_color; color: @accent_fg_color; }}
            .llm-bubble-assistant {{ background-color: alpha(currentColor, 0.1); }}
            picture.llm-thumb {{ border-radius: 8px; }}
            button.llm-action {{ min-width: 22px; min-height: 22px; padding: 2px; }}
            """
        )
        Gtk.StyleContext.add_provider_for_display(display, provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        self._card_style = (display, provider)

    def _remove_card_style(self):
        style, self._card_style = self._card_style, None
        if style is not None:
            Gtk.StyleContext.remove_provider_for_display(*style)

    # -- voice conversation ----------------------------------------------

    def _toggle_voice(self):
        if self._listener is not None:
            self._end_voice()
            self._set_status("")
            self._refresh_controls()
            return
        if self._config["asr_backend"] == "off":
            rsimgv.notify(f"{NAME}: choose a voice input backend in the plugin settings.", seconds=4)
            return
        if self._recorder is not None or self._transcribing:
            rsimgv.notify(f"{NAME}: finish the recording first.", seconds=2)
            return

        generation = self._generation
        try:
            args = self._command(self._config["vad_record_command"])
            listener = Listener(
                args,
                self._config["vad_threshold"],
                self._config["vad_silence_ms"],
                lambda hearing: rsimgv.ui.idle_add(lambda: self._voice_hearing(generation, hearing)),
                lambda pcm: self._voice_speech(generation, pcm),
                lambda: rsimgv.ui.idle_add(lambda: self._voice_ended(generation)),
            )
        except Exception as exc:
            rsimgv.notify(f"{NAME}: cannot listen: {exc}", level="error", seconds=8)
            return

        self._stop_speech()
        self._listener = listener
        self._voice_busy = False
        self._hearing = False
        if self._config["tts_backend"] == "off":
            rsimgv.notify(
                f"{NAME}: listening, but replies will not be spoken until a speech backend is set.",
                level="warning",
                seconds=6,
            )
        self._refresh_controls()

    def _end_voice(self):
        listener, self._listener = self._listener, None
        self._voice_busy = False
        self._hearing = False
        if listener is not None:
            listener.stop()

    def _voice_hearing(self, generation, hearing):
        if not self._current(generation) or self._listener is None:
            return
        self._hearing = hearing
        if hearing:
            # Speech that starts over a reply, or just after it, may be the reply itself coming back through the microphone.
            self._over_speech = (
                self._speech is not None or time.monotonic() - self._spoken_at < 1.5
            )
            if self._speech is not None and self._config["barge_in"]:
                self._stop_speech()
        self._refresh_controls()

    def _voice_speech(self, generation, pcm):
        """Called on the listener's thread when a stretch of speech ends."""
        self._voice_busy = True
        self._hearing = False
        threading.Thread(
            target=self._voice_worker, args=(generation, self._listener, pcm), daemon=True
        ).start()

    def _voice_worker(self, generation, listener, pcm):
        text, error = "", None
        directory = tempfile.mkdtemp(prefix="rsimgv-chat-")
        try:
            path = os.path.join(directory, "speech.wav")
            with wave.open(path, "wb") as file:
                file.setnchannels(1)
                file.setsampwidth(2)
                file.setframerate(Listener.RATE)
                file.writeframes(pcm)
            text = self._transcribe(path)
        except Exception as exc:
            error = str(exc)
        finally:
            shutil.rmtree(directory, ignore_errors=True)
        rsimgv.ui.idle_add(lambda: self._voice_heard(generation, listener, text, error))

    def _voice_heard(self, generation, listener, text, error):
        if not self._current(generation) or listener is None or listener is not self._listener:
            return
        self._voice_busy = False
        if error is not None:
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=8)
        elif self._is_speech(text) and not (self._over_speech and self._is_echo(text)):
            self._send(text)
        self._over_speech = False
        self._refresh_controls()

    def _voice_ended(self, generation):
        if not self._current(generation) or self._listener is None:
            return
        self._end_voice()
        self._set_status("")
        self._refresh_controls()
        rsimgv.notify(
            f"{NAME}: the microphone program stopped, so the voice conversation ended.",
            level="warning",
            seconds=8,
        )

    @staticmethod
    def _is_speech(text):
        """Whether a transcript is words, not the marker a recogniser gives for silence or noise."""
        text = text.strip()
        if not re.search(r"\w", text):
            return False
        return not re.fullmatch(r"[\[(*♪].*[\])*♪]", text)

    def _is_echo(self, text):
        """Whether a transcript is mostly a stretch of what was just spoken, so the microphone heard the speakers."""
        words = re.findall(r"\w+", text.lower())
        spoken = re.findall(r"\w+", self._last_spoken.lower())
        if len(words) < 3 or not spoken:
            return False
        match = difflib.SequenceMatcher(None, words, spoken, autojunk=False).find_longest_match(
            0, len(words), 0, len(spoken)
        )
        return match.size / len(words) >= 0.8

    def _update_voice(self):
        """Opens the listener's gate when nothing else is going on, and says what the conversation is doing."""
        listener = self._listener
        if listener is None:
            return
        free = (
            self._request is None
            and not self._voice_busy
            and not self._transcribing
            and self._recorder is None
        )
        speaking = self._speech is not None
        barge_in = self._config["barge_in"]
        if speaking:
            listener.speaking.set()
        else:
            listener.speaking.clear()
        # The microphone stays open while a reply is spoken if the user may talk over it.
        if free and (barge_in or not speaking):
            listener.gate.set()
        else:
            listener.gate.clear()

        if self._request is not None:
            return
        if self._voice_busy:
            self._set_status("Transcribing…")
        elif self._hearing:
            self._set_status("Hearing you…")
        elif speaking:
            self._set_status("Speaking… talk to interrupt" if barge_in else "Speaking…")
        elif free:
            self._set_status("Listening…")

    # -- spoken replies --------------------------------------------------

    def _toggle_chat(self):
        """Shows or hides the chat where you are: over the picture in the viewer, or in the sidebar otherwise."""
        try:
            rsimgv.host.call("view.info")
            in_viewer = True
        except Exception:
            in_viewer = False
        targets = [panel for panel in self._panels.values() if panel.in_viewer == in_viewer]
        if not targets:
            return
        # Judged by what is on screen, since the sidebar can also be closed from outside the plugin.
        # A viewer card that is fading out is still visible, so it is judged by where it is going.
        show = not any(panel.shown if in_viewer else panel.container.get_visible() for panel in targets)
        for panel in targets:
            if in_viewer:
                self._show_card(panel, show)
            else:
                panel.container.set_visible(show)
            if show:
                panel.entry.grab_focus()
        if in_viewer:
            self._chat_shown = show
        else:
            self._save_setting("chat_sidebar", show)

    def _toggle_speak(self):
        if self._config["tts_backend"] == "off":
            rsimgv.notify(f"{NAME}: choose a speech backend in the plugin settings.", seconds=4)
            return
        self._speak = not self._speak
        if not self._speak:
            self._stop_speech()
        self._refresh_controls()

    def _speak_text(self, text):
        text = spoken_text(text)[:MAX_SPOKEN]
        if not text:
            return
        speech = Speech()
        self._speech = speech
        self._last_spoken = text
        self._spoken_at = time.monotonic()
        self._refresh_controls()
        threading.Thread(
            target=self._speech_worker, args=(self._generation, speech, text), daemon=True
        ).start()

    def _stop_speech(self):
        speech, self._speech = self._speech, None
        if speech is not None:
            speech.stop()
            self._spoken_at = time.monotonic()
            self._refresh_controls()

    def _speech_worker(self, generation, speech, text):
        config = self._config
        directory = tempfile.mkdtemp(prefix="rsimgv-chat-")
        path = os.path.join(directory, "reply.wav")
        error = None
        try:
            if config["tts_backend"] == "command":
                self._run_speech(
                    speech, self._command(config["tts_command"], output=path), text.encode("utf-8")
                )
            else:
                self._synthesize(text, path)
            if not speech.cancelled:
                if not os.path.isfile(path) or os.path.getsize(path) == 0:
                    raise RuntimeError("the speech backend produced no audio")
                self._run_speech(speech, self._command(config["play_command"], input=path), None)
        except Exception as exc:
            if not speech.cancelled:
                error = str(exc)
        finally:
            shutil.rmtree(directory, ignore_errors=True)

        rsimgv.ui.idle_add(lambda: self._speech_done(generation, speech, error))

    def _synthesize(self, text, path):
        config = self._config
        base = (config["tts_url"] or config["base_url"]).rstrip("/")
        body = {
            "model": config["tts_model"],
            "voice": config["tts_voice"],
            "input": text,
            "response_format": "wav",
        }
        api_key = os.environ.get(config["api_key_env"], "").strip()
        with self._open(
            f"{base}/audio/speech",
            json.dumps(body).encode("utf-8"),
            {"Content-Type": "application/json"},
            api_key,
        ) as response, open(path, "wb") as file:
            shutil.copyfileobj(response, file)

    def _run_speech(self, speech, args, stdin_data):
        """Runs a program that Speech.stop can end, and raises if it fails."""
        with speech.lock:
            if speech.cancelled:
                return
            speech.proc = subprocess.Popen(
                args,
                stdin=subprocess.PIPE if stdin_data is not None else subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            proc = speech.proc
        try:
            _, stderr = proc.communicate(stdin_data, timeout=self._config["timeout_seconds"] * 5)
        except subprocess.TimeoutExpired as exc:
            proc.kill()
            proc.communicate()
            raise RuntimeError(f"{args[0]} took too long") from exc
        if proc.returncode != 0 and not speech.cancelled:
            detail = (stderr or b"").decode("utf-8", "replace").strip()
            raise RuntimeError(f"{args[0]} failed: {detail[-300:]}")

    def _speech_done(self, generation, speech, error):
        if not self._current(generation):
            return
        if self._speech is speech:
            self._speech = None
            self._spoken_at = time.monotonic()
        if error is not None:
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=8)
        self._refresh_controls()

    # -- shared helpers --------------------------------------------------

    def _current(self, generation):
        return self._active and generation == self._generation

    @staticmethod
    def _command(template, **values):
        """A command line split into arguments, with {name} replaced in each one."""
        args = shlex.split(template)
        if not args:
            raise RuntimeError("the command is empty; set it in the plugin settings")
        for name, value in values.items():
            args = [arg.replace("{" + name + "}", value) for arg in args]
        return args

    def _open(self, url, data, headers, api_key):
        """Opens a POST request, turning HTTP and network failures into readable errors."""
        headers = dict(headers)
        if api_key:
            headers["Authorization"] = f"Bearer {api_key}"
        request = urllib.request.Request(url, data=data, headers=headers, method="POST")
        try:
            return urllib.request.urlopen(request, timeout=self._config["timeout_seconds"])
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", "replace")
            try:
                message = self._error_message(json.loads(detail).get("error", detail))
            except Exception:
                message = detail
            raise RuntimeError(f"HTTP {exc.code}: {message[:300]}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"request failed: {exc.reason}") from exc

    @staticmethod
    def _error_message(error):
        if isinstance(error, dict):
            return str(error.get("message", error))
        return str(error)

    # -- panel updates ---------------------------------------------------

    def _add_bubble(self, panel, role, text, filename, thinking="", live=False, message=None):
        """Appends a message to a panel's log and returns it. `message` is its entry in the history."""
        message = message or {}
        name = (self._config["user_name"].strip() or "User") if role == "user" else "Assistant"
        bubble = Bubble(
            role, text, thinking, name, self, live, message.get("id"), message.get("thumb"), message.get("path"),
            message.get("notes"),
        )
        panel.log.append(bubble.box)
        panel.bubbles.append(bubble)
        return bubble

    def _show_file(self, filename):
        for panel in self._panels.values():
            panel.file_label.set_text(filename or "No picture open")

    def _set_status(self, text):
        for panel in self._panels.values():
            panel.status.set_text(text or " ")

    def _refresh_controls(self):
        busy = self._request is not None
        recording = self._recorder is not None
        speaking = self._speech is not None
        can_speak = self._config["tts_backend"] != "off"
        for panel in self._panels.values():
            panel.entry.set_sensitive(not recording and not self._transcribing)
            panel.send_button.set_icon_name(
                "media-playback-stop-symbolic" if busy else "mail-send-symbolic"
            )
            panel.send_button.set_tooltip_text("Stop" if busy else "Send")
            panel.mic_button.set_sensitive(not busy and not self._transcribing and self._listener is None)
            panel.mic_button.set_icon_name(
                "media-record-symbolic" if recording else "audio-input-microphone-symbolic"
            )
            if recording:
                panel.mic_button.add_css_class("destructive-action")
            else:
                panel.mic_button.remove_css_class("destructive-action")
            panel.speak_button.set_icon_name(
                "audio-volume-high-symbolic" if self._speak else "audio-volume-muted-symbolic"
            )
            panel.speak_button.set_tooltip_text(
                "Spoken replies are on" if self._speak else "Spoken replies are off"
            )
            panel.stop_speaking_button.set_visible(speaking)
            panel.thinking_button.set_tooltip_text(
                "The model may think before it answers" if self._thinking else "The model is asked not to think"
            )
            if self._thinking:
                panel.thinking_button.add_css_class("suggested-action")
            else:
                panel.thinking_button.remove_css_class("suggested-action")
            panel.voice_button.set_tooltip_text(
                "End the voice conversation" if self._listener else "Start a voice conversation"
            )
            if self._listener:
                panel.voice_button.add_css_class("suggested-action")
            else:
                panel.voice_button.remove_css_class("suggested-action")

            # Only the last reply can be asked for again, and only while nothing is pending.
            last = panel.bubbles[-1] if panel.bubbles else None
            for bubble in panel.bubbles:
                finished = bubble is not panel.streaming
                assistant = bubble.role == "assistant"
                bubble.set_actions(
                    replay=assistant and finished and can_speak and not bubble.failed,
                    regenerate=assistant and finished and not busy and bubble is last,
                    delete=finished and not busy,
                )
        self._update_voice()

    def _clear(self):
        self._cancel_request()
        self._stop_speech()
        self._history = []
        self._last_image = None
        self._save_history()
        for panel in self._panels.values():
            for bubble in panel.bubbles:
                panel.log.remove(bubble.box)
            panel.bubbles = []
            panel.streaming = None

    # -- storage ---------------------------------------------------------

    def _trim_history(self):
        if len(self._history) > MAX_STORED:
            self._history = self._history[-MAX_STORED:]

    def _load_history(self):
        try:
            with open(self._history_path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
        except FileNotFoundError:
            return []
        except Exception as exc:
            rsimgv.notify(
                f"{NAME}: cannot read {self._history_path}: {exc}", level="warning", seconds=8
            )
            return []

        history = []
        for message in loaded if isinstance(loaded, list) else []:
            if (
                isinstance(message, dict)
                and message.get("role") in ("user", "assistant")
                and isinstance(message.get("text"), str)
            ):
                message.setdefault("id", uuid.uuid4().hex)
                history.append(message)
        return history

    def _save_history(self):
        temporary = f"{self._history_path}.tmp"
        try:
            os.makedirs(os.path.dirname(self._history_path), exist_ok=True)
            with open(temporary, "w", encoding="utf-8") as file:
                json.dump(self._history, file, ensure_ascii=False)
            os.replace(temporary, self._history_path)
        except OSError as exc:
            rsimgv.notify(f"{NAME}: cannot save the conversation: {exc}", level="warning", seconds=8)

    def _load_config(self, config_path):
        config = dict(DEFAULTS)
        path = os.path.join(os.path.dirname(config_path), "config.json")
        try:
            with open(path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
            if isinstance(loaded, dict):
                config.update({key: value for key, value in loaded.items() if key in DEFAULTS})
        except FileNotFoundError:
            pass
        except Exception as exc:
            rsimgv.notify(f"{NAME}: cannot read {path}: {exc}", level="warning", seconds=8)

        for key, default in DEFAULTS.items():
            value = config[key]
            if isinstance(default, bool):
                config[key] = bool(value)
            elif isinstance(default, int):
                low, high = LIMITS[key]
                try:
                    config[key] = max(low, min(high, int(value)))
                except (TypeError, ValueError):
                    config[key] = default
            else:
                config[key] = str(value)
        for key in ("asr_backend", "tts_backend"):
            if config[key] not in BACKENDS:
                config[key] = "off"
        return config
