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
import collections
import copy
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
gi.require_version("Pango", "1.0")
gi.require_version("Graphene", "1.0")
from gi.repository import Gdk, Graphene, Gtk, Pango

import rsimgv


NAME = "LLM Chat"

# The API rejects very large requests, and a base64 PNG grows quickly.
MAX_BASE64 = 20_000_000
# The most messages kept on disk. Older ones are dropped.
MAX_STORED = 500
# OpenAI's speech endpoint takes at most 4096 characters.
MAX_SPOKEN = 4000
# How near the bottom, in pixels, the log counts as following new messages.
FOLLOW_SLACK = 24

# The viewer card's size when it has not been resized, and the smallest it can be made.
CARD_SIZE = (380, 480)
CARD_MIN = (280, 220)

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
}

LIMITS = {
    "history_messages": (1, 200),
    "max_tokens": (1, 32768),
    "timeout_seconds": (5, 600),
    "vad_threshold": (50, 8000),
    "vad_silence_ms": (200, 5000),
    "card_opacity": (20, 100),
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

    def __init__(self, args, threshold, silence_ms, on_hearing, on_speech, on_ended):
        self.threshold = threshold
        self.silence_ms = silence_ms
        self.on_hearing = on_hearing
        self.on_speech = on_speech
        self.on_ended = on_ended
        self.gate = threading.Event()
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
                threshold = max(self.threshold, floor * 3)
                if speech is None:
                    preroll.append(frame)
                    if level >= threshold:
                        loud += 1
                    else:
                        loud = 0
                        floor = min(2000.0, max(20.0, floor * 0.98 + level * 0.02))
                    if loud >= self.START_FRAMES:
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
    """One message in a panel's log."""

    def __init__(self, role, text, thinking, filename, replay, regenerate, live=False):
        self.role = role
        # A live bubble is being written now, so it can say how long the model thought.
        self._live = live
        self.text = text
        self.thinking = thinking
        self.failed = False
        self._started = time.monotonic()
        self._thinking_shown = False
        self._collapsed = False

        self.box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        self.box.set_margin_start(6)
        self.box.set_margin_end(6)

        self.caption = Gtk.Label(label="You" if role == "user" else "Assistant", xalign=0.0, hexpand=True)
        self.caption.add_css_class("caption-heading")
        self.caption.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        if role == "user":
            self.caption.add_css_class("accent")
            if filename:
                self.caption.set_label(f"You · {filename}")

        head = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        head.append(self.caption)
        self.replay_button = Gtk.Button.new_from_icon_name("audio-volume-high-symbolic")
        self.regenerate_button = Gtk.Button.new_from_icon_name("view-refresh-symbolic")
        for button, tip, callback in (
            (self.replay_button, "Read aloud", lambda *_: replay(self)),
            (self.regenerate_button, "Regenerate", lambda *_: regenerate()),
        ):
            button.add_css_class("flat")
            button.add_css_class("circular")
            button.set_tooltip_text(tip)
            button.set_visible(False)
            button.connect("clicked", callback)
            if role == "assistant":
                head.append(button)

        self.think_label = Gtk.Label(xalign=0.0, hexpand=True)
        self.think_label.set_wrap(True)
        self.think_label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.think_label.set_selectable(True)
        self.think_label.add_css_class("dim-label")
        self.expander = Gtk.Expander(label="Thinking…")
        self.expander.set_child(self.think_label)
        self.expander.set_visible(False)

        self.body = Gtk.Label(xalign=0.0, hexpand=True)
        self.body.set_wrap(True)
        self.body.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        self.body.set_selectable(True)

        self.box.append(head)
        self.box.append(self.expander)
        self.box.append(self.body)
        self.show(thinking, text, final=True)

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
        self.stop_speaking_button = None
        self.bubbles = []
        self.streaming = None
        # Whether new messages scroll into view. It is off while the reader is scrolled up.
        self.follow = True
        # Whether the viewer card is placed by margins from the top left rather than by the host's corner.
        self.anchored = False


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
        self._config_file = None
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
        self._config = self._load_config(config_path)
        self._parse_parameters()
        self._speak = bool(self._config["auto_speak"]) and self._config["tts_backend"] != "off"
        self._history = self._load_history()

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
        clear_button.set_tooltip_text("Clear the conversation")

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=2)
        header.append(panel.file_label)
        header.append(panel.stop_speaking_button)
        header.append(panel.thinking_button)
        header.append(panel.voice_button)
        header.append(panel.speak_button)
        header.append(clear_button)

        panel.log = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        panel.log.set_margin_top(4)
        panel.log.set_margin_bottom(4)

        panel.scroll = Gtk.ScrolledWindow(vexpand=True)
        panel.scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        panel.scroll.set_min_content_height(80 if panel.in_viewer else 260)
        panel.scroll.set_child(panel.log)

        panel.status = Gtk.Label(xalign=0.0, hexpand=True)
        panel.status.add_css_class("dim-label")
        panel.status.add_css_class("caption")
        panel.status.set_visible(False)

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
        handle = None
        if panel.in_viewer:
            handle = Gtk.Label(label="◢")
            handle.add_css_class("dim-label")
            handle.set_tooltip_text("Drag to resize")
            handle.set_cursor_from_name("nwse-resize")
            footer.append(handle)

        container.append(header)
        container.append(panel.scroll)
        container.append(footer)
        container.append(row)

        rsimgv.ui.connect(panel.send_button, "clicked", lambda *_: self._send_or_stop(panel))
        rsimgv.ui.connect(panel.entry, "activate", lambda *_: self._send_from(panel))
        rsimgv.ui.connect(panel.mic_button, "clicked", lambda *_: self._toggle_record(panel))
        rsimgv.ui.connect(panel.speak_button, "clicked", lambda *_: self._toggle_speak())
        rsimgv.ui.connect(panel.voice_button, "clicked", lambda *_: self._toggle_voice())
        rsimgv.ui.connect(panel.thinking_button, "clicked", lambda *_: self._toggle_thinking())
        rsimgv.ui.connect(panel.stop_speaking_button, "clicked", lambda *_: self._stop_speech())
        rsimgv.ui.connect(clear_button, "clicked", lambda *_: self._clear())

        # The log follows new messages while it is at the bottom, and stays put once the reader scrolls up.
        adjustment = panel.scroll.get_vadjustment()
        rsimgv.ui.connect(adjustment, "value-changed", lambda a: self._scrolled(panel, a))
        rsimgv.ui.connect(adjustment, "changed", lambda a: self._grew(panel, a))

        if panel.in_viewer:
            self._apply_card_style(container)
            self._place_card(panel, header, handle)
            container.set_visible(self._chat_shown)
            for child in (header, panel.scroll, footer, row):
                child.set_margin_start(8)
                child.set_margin_end(8)
            header.set_margin_top(8)
            row.set_margin_bottom(8)

        self._panels[panel.key] = panel
        for message in self._history:
            self._add_bubble(
                panel, message["role"], message["text"], message.get("file"), message.get("thinking", "")
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

    def _place_card(self, panel, header, handle):
        """Sets the card's saved size and place, and lets its header move it and its corner resize it."""
        container = panel.container
        layout = self._load_layout()
        width, height = layout.get("w", CARD_SIZE[0]), layout.get("h", CARD_SIZE[1])
        container.set_size_request(max(CARD_MIN[0], width), max(CARD_MIN[1], height))
        if "x" in layout and "y" in layout:
            self._anchor(panel, layout["x"], layout["y"])
            # The overlay's size is known once it is shown, so keep the card inside it then.
            rsimgv.ui.idle_add(lambda: self._clamp_card(panel))

        header.set_cursor_from_name("move")
        move = Gtk.GestureDrag()
        move.set_button(1)
        state = {}

        def move_begin(gesture, x, y):
            overlay = container.get_parent()
            if overlay is None:
                return
            self._anchor_here(panel)
            state["start"] = (container.get_margin_start(), container.get_margin_top())
            state["local"] = (x, y)
            state["pointer"] = self._in_overlay(header, overlay, x, y)

        def move_update(gesture, dx, dy):
            overlay = container.get_parent()
            if overlay is None or "start" not in state:
                return
            px, py = self._in_overlay(header, overlay, state["local"][0] + dx, state["local"][1] + dy)
            self._move_card(
                panel,
                state["start"][0] + px - state["pointer"][0],
                state["start"][1] + py - state["pointer"][1],
            )

        rsimgv.ui.connect(move, "drag-begin", move_begin)
        rsimgv.ui.connect(move, "drag-update", move_update)
        rsimgv.ui.connect(move, "drag-end", lambda *_: self._save_layout(panel))
        header.add_controller(move)

        # A double click puts the card back in its corner at its first size.
        reset = Gtk.GestureClick()
        reset.set_button(1)
        rsimgv.ui.connect(
            reset, "pressed", lambda g, presses, x, y: self._reset_card(panel) if presses == 2 else None
        )
        header.add_controller(reset)

        resize = Gtk.GestureDrag()
        resize.set_button(1)
        size = {}

        def resize_begin(gesture, x, y):
            overlay = container.get_parent()
            if overlay is None:
                return
            self._anchor_here(panel)
            size["start"] = (container.get_width(), container.get_height())
            size["local"] = (x, y)
            size["pointer"] = self._in_overlay(handle, overlay, x, y)

        def resize_update(gesture, dx, dy):
            overlay = container.get_parent()
            if overlay is None or "start" not in size:
                return
            px, py = self._in_overlay(handle, overlay, size["local"][0] + dx, size["local"][1] + dy)
            left, top = container.get_margin_start(), container.get_margin_top()
            width = size["start"][0] + px - size["pointer"][0]
            height = size["start"][1] + py - size["pointer"][1]
            width = max(CARD_MIN[0], min(width, overlay.get_width() - left))
            height = max(CARD_MIN[1], min(height, overlay.get_height() - top))
            container.set_size_request(int(width), int(height))

        rsimgv.ui.connect(resize, "drag-begin", resize_begin)
        rsimgv.ui.connect(resize, "drag-update", resize_update)
        rsimgv.ui.connect(resize, "drag-end", lambda *_: self._save_layout(panel))
        handle.add_controller(resize)

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
        container.set_size_request(max(CARD_MIN[0], width), max(CARD_MIN[1], height))

    def _move_card(self, panel, x, y):
        container, overlay = panel.container, panel.container.get_parent()
        if overlay is not None:
            x = min(x, overlay.get_width() - container.get_width())
            y = min(y, overlay.get_height() - container.get_height())
        container.set_margin_start(int(max(0, x)))
        container.set_margin_top(int(max(0, y)))

    def _clamp_card(self, panel):
        if panel.anchored and panel.container is not None and panel.container.get_parent() is not None:
            self._move_card(panel, panel.container.get_margin_start(), panel.container.get_margin_top())

    def _reset_card(self, panel):
        container = panel.container
        container.set_halign(Gtk.Align.END)
        container.set_valign(Gtk.Align.START)
        for margin in ("start", "top", "end", "bottom"):
            getattr(container, f"set_margin_{margin}")(12)
        container.set_size_request(*CARD_SIZE)
        panel.anchored = False
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
        return {
            key: int(value)
            for key, value in loaded.items()
            if key in ("x", "y", "w", "h") and isinstance(value, (int, float))
        }

    def _save_layout(self, panel):
        container = panel.container
        layout = {
            "x": container.get_margin_start(),
            "y": container.get_margin_top(),
            "w": container.get_size_request()[0],
            "h": container.get_size_request()[1],
        }
        try:
            os.makedirs(os.path.dirname(self._layout_path), exist_ok=True)
            with open(self._layout_path, "w", encoding="utf-8") as file:
                json.dump(layout, file)
        except OSError as exc:
            rsimgv.notify(f"{NAME}: cannot save the chat's place: {exc}", level="warning", seconds=6)

    # -- following new messages ------------------------------------------

    def _scrolled(self, panel, adjustment):
        distance = adjustment.get_upper() - adjustment.get_value() - adjustment.get_page_size()
        panel.follow = distance <= FOLLOW_SLACK

    def _grew(self, panel, adjustment):
        if panel.follow:
            adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())

    def _scroll_down(self, panel):
        """Follows the log to its end, as when the reader has just sent a message."""
        panel.follow = True
        adjustment = panel.scroll.get_vadjustment()
        adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())

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
        context, filename, image = self._gather_context()
        message = {
            "id": uuid.uuid4().hex,
            "role": "user",
            "text": text,
            "file": filename,
            "context": context,
        }
        self._history.append(message)
        self._trim_history()
        self._save_history()
        self._last_image = (message["id"], image)
        for panel in self._panels.values():
            self._add_bubble(panel, "user", text, filename)
            self._scroll_down(panel)
        self._begin_request(image)
        return True

    def _begin_request(self, image):
        """Starts a request for the conversation as it stands, which ends with a user message."""
        request = Request()
        self._request = request
        for panel in self._panels.values():
            panel.streaming = self._add_bubble(panel, "assistant", "", None, live=True)
        self._refresh_controls()
        self._set_status("Waiting for the reply…")

        api_key = os.environ.get(self._config["api_key_env"], "").strip()
        body = self._request_body(self._build_messages(image))
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
            _, filename, current = self._gather_context()
            if filename == user.get("file"):
                image = current
        self._begin_request(image)

    def _gather_context(self):
        """The text, filename and screenshot for the item the viewer shows, on the main thread."""
        config = self._config
        try:
            info = rsimgv.host.call("view.info")
        except Exception:
            return "No picture or video is open in the viewer.", "", None

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
                if len(data) > MAX_BASE64:
                    lines.append("The picture is too large to send.")
                else:
                    image = data
        return "\n".join(lines), filename, image

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
                        {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{image}"}},
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
        self._finish_bubbles(thinking, reply + ("\n\n(stopped)" if reply else "(stopped)"))
        if reply.strip():
            self._remember_reply(thinking, reply)
        self._set_status("")
        self._refresh_controls()

    def _request_body(self, messages):
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
        rsimgv.ui.idle_add(lambda: self._chat_done(generation, request, error))

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
            if changed:
                self._post_partial(generation, request)

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
        content = message.get("content")
        if isinstance(content, list):
            content = "\n".join(
                part.get("text", "") for part in content if isinstance(part, dict) and part.get("text")
            )
        if content is None and request.thinking:
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
        for panel in self._panels.values():
            if panel.streaming is not None:
                panel.streaming.show(thinking, reply)
        self._set_status("" if reply else ("Thinking…" if thinking else "Waiting for the reply…"))

    def _chat_done(self, generation, request, error):
        if not self._current(generation) or self._request is not request:
            return
        self._request = None
        thinking, reply = request.texts()
        thinking, reply = thinking.strip(), reply.strip()

        if error is not None and not reply:
            self._finish_bubbles(thinking, error, failed=True)
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=10)
        elif not reply:
            self._finish_bubbles(thinking, "(the model returned an empty reply)", failed=True)
        else:
            self._finish_bubbles(thinking, reply)
            self._remember_reply(thinking, reply)
            if error is not None:
                rsimgv.notify(f"{NAME}: the reply was cut short: {error}", level="warning", seconds=8)
            elif (self._speak or self._listener is not None) and self._config["tts_backend"] != "off":
                self._speak_text(reply)

        self._set_status("")
        self._refresh_controls()

    def _remember_reply(self, thinking, reply):
        message = {"id": uuid.uuid4().hex, "role": "assistant", "text": reply.strip()}
        if thinking.strip():
            message["thinking"] = thinking.strip()
        self._history.append(message)
        self._trim_history()
        self._save_history()

    def _finish_bubbles(self, thinking, text, failed=False):
        for panel in self._panels.values():
            bubble = panel.streaming
            panel.streaming = None
            if bubble is not None:
                bubble.show(thinking, text, final=True, failed=failed)

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

    # -- the viewer card's background ------------------------------------

    def _apply_card_style(self, container):
        """Gives the viewer card the background opacity from the settings.

        Only the card's own background is made see-through, not its text, so a
        rule for one class is added for the display rather than fading the widget.
        """
        container.add_css_class("llm-chat-card")
        if self._card_style is not None:
            return
        opacity = self._config["card_opacity"] / 100
        provider = Gtk.CssProvider()
        provider.load_from_string(
            f".llm-chat-card {{ background-color: alpha(@window_bg_color, {opacity:.2f}); }}"
        )
        display = Gdk.Display.get_default()
        if display is None:
            return
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
        elif self._is_speech(text):
            self._send(text)
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

    def _update_voice(self):
        """Opens the listener's gate when nothing else is going on, and says what the conversation is doing."""
        listener = self._listener
        if listener is None:
            return
        idle = (
            self._request is None
            and self._speech is None
            and not self._voice_busy
            and not self._transcribing
            and self._recorder is None
        )
        if idle:
            listener.gate.set()
        else:
            listener.gate.clear()

        if self._request is not None:
            return
        if self._voice_busy:
            self._set_status("Transcribing…")
        elif self._speech is not None:
            self._set_status("Speaking…")
        elif self._hearing:
            self._set_status("Hearing you…")
        elif idle:
            self._set_status("Listening…")

    # -- spoken replies --------------------------------------------------

    def _toggle_chat(self):
        """Shows or hides the chat over the viewer's picture. The sidebar chat is not affected."""
        self._chat_shown = not self._chat_shown
        for panel in self._panels.values():
            if panel.in_viewer:
                panel.container.set_visible(self._chat_shown)
                if self._chat_shown:
                    panel.entry.grab_focus()

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
        self._refresh_controls()
        threading.Thread(
            target=self._speech_worker, args=(self._generation, speech, text), daemon=True
        ).start()

    def _stop_speech(self):
        speech, self._speech = self._speech, None
        if speech is not None:
            speech.stop()
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

    def _add_bubble(self, panel, role, text, filename, thinking="", live=False):
        """Appends a message to a panel's log and returns it."""
        bubble = Bubble(role, text, thinking, filename, self._replay, self._regenerate, live)
        panel.log.append(bubble.box)
        panel.bubbles.append(bubble)
        return bubble

    def _show_file(self, filename):
        for panel in self._panels.values():
            panel.file_label.set_text(filename or "No picture open")

    def _set_status(self, text):
        for panel in self._panels.values():
            panel.status.set_text(text)
            panel.status.set_visible(bool(text))

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
                if bubble.role != "assistant":
                    continue
                finished = bubble is not panel.streaming
                bubble.replay_button.set_visible(finished and can_speak and not bubble.failed)
                bubble.regenerate_button.set_visible(finished and not busy and bubble is last)
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
