"""LLM Chat: a persistent chat in the sidebar about the picture being viewed.

Every message is sent with the viewer's current picture or video frame, its
filename and its metadata. The conversation is kept in a file beside the
plugin's config and restored when rsimgv starts. Voice input records with an
external program and transcribes with an HTTP endpoint or a local command;
spoken replies are synthesized the same way and played by an external program.

Widget signals are connected in ``create_panel``, the one place the host gives
the plugin a context that ``rsimgv.ui.connect`` accepts. Network requests and
audio programs run on worker threads, and their results are posted back with
``rsimgv.ui.idle_add``, which runs on the GTK main thread.
"""

import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import threading
import urllib.error
import urllib.request
import uuid

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Pango", "1.0")
from gi.repository import Gtk, Pango

import rsimgv


NAME = "LLM Chat"

# The API rejects very large requests, and a base64 PNG grows quickly.
MAX_BASE64 = 20_000_000
# The most messages kept on disk. Older ones are dropped.
MAX_STORED = 500
# OpenAI's speech endpoint takes at most 4096 characters.
MAX_SPOKEN = 4000

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
}

LIMITS = {
    "history_messages": (1, 200),
    "max_tokens": (1, 32768),
    "timeout_seconds": (5, 600),
}

BACKENDS = ("off", "http", "command")


class Request:
    """One chat request. Replies from a request that is no longer current are ignored."""

    def __init__(self):
        self.cancel = threading.Event()
        self.parts = []


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


class Panel:
    """The widgets of one chat panel. Each window has its own."""

    def __init__(self, panel_id, instance_id):
        self.key = (panel_id, instance_id)
        self.file_label = None
        self.scroll = None
        self.log = None
        self.status = None
        self.entry = None
        self.send_button = None
        self.mic_button = None
        self.speak_button = None
        self.stop_speaking_button = None
        self.streaming = None


class Plugin:
    def __init__(self):
        self._active = True
        self._generation = None
        self._config = dict(DEFAULTS)
        self._history_path = None
        self._history = []
        self._panels = {}
        self._request = None
        self._speech = None
        self._speak = False
        self._recorder = None
        self._record_dir = None
        self._record_panel = None
        self._transcribing = False

    def activate(self, host):
        self._active = True
        self._generation = rsimgv.host.generation
        config_path = rsimgv.host.config_path()
        self._history_path = os.path.join(os.path.dirname(config_path), "history.json")
        self._config = self._load_config(config_path)
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
        self._panels = {}

    # -- host callbacks --------------------------------------------------

    def invoke_action(self, action, context):
        if action == "speak-toggle":
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
        if panel_id != "chat":
            return

        panel = Panel(panel_id, instance_id)
        config = self._config

        panel.file_label = Gtk.Label(xalign=0.0, hexpand=True)
        panel.file_label.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        panel.file_label.add_css_class("dim-label")
        panel.file_label.add_css_class("caption")

        panel.speak_button = Gtk.Button.new_from_icon_name("audio-volume-muted-symbolic")
        panel.speak_button.add_css_class("flat")
        panel.speak_button.set_visible(config["tts_backend"] != "off")

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
        header.append(panel.speak_button)
        header.append(clear_button)

        panel.log = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        panel.log.set_margin_top(4)
        panel.log.set_margin_bottom(4)

        panel.scroll = Gtk.ScrolledWindow(vexpand=True)
        panel.scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        panel.scroll.set_min_content_height(260)
        panel.scroll.set_child(panel.log)

        panel.status = Gtk.Label(xalign=0.0)
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

        container.append(header)
        container.append(panel.scroll)
        container.append(panel.status)
        container.append(row)

        rsimgv.ui.connect(panel.send_button, "clicked", lambda *_: self._send_or_stop(panel))
        rsimgv.ui.connect(panel.entry, "activate", lambda *_: self._send_from(panel))
        rsimgv.ui.connect(panel.mic_button, "clicked", lambda *_: self._toggle_record(panel))
        rsimgv.ui.connect(panel.speak_button, "clicked", lambda *_: self._toggle_speak())
        rsimgv.ui.connect(panel.stop_speaking_button, "clicked", lambda *_: self._stop_speech())
        rsimgv.ui.connect(clear_button, "clicked", lambda *_: self._clear())

        self._panels[panel.key] = panel
        for message in self._history:
            self._add_bubble(panel, message["role"], message["text"], message.get("file"))
        self._refresh_controls()
        self._scroll_down(panel)

        try:
            info = rsimgv.host.call("view.info")
            self._show_file(info.get("filename") or "")
        except Exception:
            self._show_file("")

    def panel_destroyed(self, panel_id, instance_id):
        self._panels.pop((panel_id, instance_id), None)

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
        self._history.append({"role": "user", "text": text, "file": filename, "context": context})
        self._trim_history()
        self._save_history()
        for panel in self._panels.values():
            self._add_bubble(panel, "user", text, filename)

        request = Request()
        self._request = request
        for panel in self._panels.values():
            panel.streaming = self._add_bubble(panel, "assistant", "…", None)
            self._scroll_down(panel)
        self._refresh_controls()
        self._set_status("Waiting for the reply…")

        api_key = os.environ.get(self._config["api_key_env"], "").strip()
        messages = self._build_messages(image)
        threading.Thread(
            target=self._chat_worker,
            args=(self._generation, request, api_key, messages),
            daemon=True,
        ).start()
        return True

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
        text = "".join(request.parts)
        self._finish_bubbles(text + ("\n\n(stopped)" if text else "(stopped)"))
        if text:
            self._history.append({"role": "assistant", "text": text})
            self._trim_history()
            self._save_history()
        self._set_status("")
        self._refresh_controls()

    def _chat_worker(self, generation, request, api_key, messages):
        config = self._config
        body = {
            "model": config["model"],
            "max_tokens": config["max_tokens"],
            "messages": messages,
        }
        if config["stream"]:
            body["stream"] = True

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
                    text = self._read_reply(json.loads(response.read().decode("utf-8", "replace")))
                    request.parts.append(text)
                    self._post_partial(generation, request)
            error = None
        except Exception as exc:
            error = str(exc)

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
            if isinstance(chunk, dict) and chunk.get("error"):
                raise RuntimeError(self._error_message(chunk["error"]))
            for choice in (chunk.get("choices") or []) if isinstance(chunk, dict) else []:
                piece = (choice.get("delta") or {}).get("content")
                if isinstance(piece, str) and piece:
                    request.parts.append(piece)
                    self._post_partial(generation, request)

    def _post_partial(self, generation, request):
        text = "".join(request.parts)
        rsimgv.ui.idle_add(lambda: self._partial(generation, request, text))

    @staticmethod
    def _read_reply(value):
        if isinstance(value, dict) and value.get("error"):
            raise RuntimeError(Plugin._error_message(value["error"]))
        choices = value.get("choices") or []
        if not choices:
            raise RuntimeError("the response has no choices")
        content = choices[0].get("message", {}).get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts = [part.get("text", "") for part in content if isinstance(part, dict)]
            return "\n".join(part for part in parts if part)
        raise RuntimeError("the response has no text")

    def _partial(self, generation, request, text):
        if not self._current(generation) or self._request is not request:
            return
        for panel in self._panels.values():
            if panel.streaming is not None:
                panel.streaming.set_text(text)
                self._scroll_down(panel)
        self._set_status("")

    def _chat_done(self, generation, request, error):
        if not self._current(generation) or self._request is not request:
            return
        self._request = None
        text = "".join(request.parts).strip()

        if error is not None and not text:
            self._finish_bubbles(error, failed=True)
            rsimgv.notify(f"{NAME}: {error}", level="error", seconds=10)
        elif not text:
            self._finish_bubbles("(the model returned an empty reply)", failed=True)
        else:
            self._finish_bubbles(text)
            self._history.append({"role": "assistant", "text": text})
            self._trim_history()
            self._save_history()
            if error is not None:
                rsimgv.notify(f"{NAME}: the reply was cut short: {error}", level="warning", seconds=8)
            elif self._speak:
                self._speak_text(text)

        self._set_status("")
        self._refresh_controls()

    def _finish_bubbles(self, text, failed=False):
        for panel in self._panels.values():
            label = panel.streaming
            panel.streaming = None
            if label is None:
                continue
            label.set_text(text)
            if failed:
                label.add_css_class("error")
            self._scroll_down(panel)

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

    # -- spoken replies --------------------------------------------------

    def _toggle_speak(self):
        if self._config["tts_backend"] == "off":
            rsimgv.notify(f"{NAME}: choose a speech backend in the plugin settings.", seconds=4)
            return
        self._speak = not self._speak
        if not self._speak:
            self._stop_speech()
        self._refresh_controls()

    def _speak_text(self, text):
        text = self._plain(text)[:MAX_SPOKEN]
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

    @staticmethod
    def _plain(text):
        """Markdown reduced to what reads well aloud."""
        text = re.sub(r"```.*?```", " ", text, flags=re.S)
        text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
        text = re.sub(r"`([^`]*)`", r"\1", text)
        text = re.sub(r"^\s{0,3}#{1,6}\s*", "", text, flags=re.M)
        text = re.sub(r"[*_~]{1,3}", "", text)
        return re.sub(r"\n{3,}", "\n\n", text).strip()

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

    def _add_bubble(self, panel, role, text, filename):
        """Appends a message to a panel's log and returns its text label."""
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        box.set_margin_start(6)
        box.set_margin_end(6)

        caption = Gtk.Label(label="You" if role == "user" else "Assistant", xalign=0.0)
        caption.add_css_class("caption-heading")
        if role == "user":
            caption.add_css_class("accent")
            if filename:
                caption.set_label(f"You · {filename}")
                caption.set_ellipsize(Pango.EllipsizeMode.MIDDLE)

        label = Gtk.Label(label=text, xalign=0.0, hexpand=True)
        label.set_wrap(True)
        label.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
        label.set_selectable(True)

        box.append(caption)
        box.append(label)
        panel.log.append(box)
        return label

    def _scroll_down(self, panel):
        def scroll():
            adjustment = panel.scroll.get_vadjustment()
            adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())

        # The log has not been laid out yet, so wait for the next pass.
        rsimgv.ui.idle_add(scroll)

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
        for panel in self._panels.values():
            panel.entry.set_sensitive(not recording and not self._transcribing)
            panel.send_button.set_icon_name(
                "media-playback-stop-symbolic" if busy else "mail-send-symbolic"
            )
            panel.send_button.set_tooltip_text("Stop" if busy else "Send")
            panel.mic_button.set_sensitive(not busy and not self._transcribing)
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

    def _clear(self):
        self._cancel_request()
        self._stop_speech()
        self._history = []
        self._save_history()
        for panel in self._panels.values():
            child = panel.log.get_first_child()
            while child is not None:
                following = child.get_next_sibling()
                panel.log.remove(child)
                child = following
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
