"""LLM Screenshot: send the viewer's current media to a vision model.

The plugin calls the host's ``view.screenshot`` method, wraps the PNG in an
OpenAI-compatible chat completions request, and shows the reply in a panel.

Written against the planned ``rsimgv`` plugin module. All callbacks run on the
GTK main thread; the network request runs on a worker thread and posts its
result back with ``rsimgv.ui.idle_add``.
"""

import json
import os
import threading
import urllib.error
import urllib.request

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk

import rsimgv


# The API rejects very large requests, and a base64 PNG can grow quickly.
MAX_BASE64 = 20_000_000

DEFAULTS = {
    "base_url": "https://api.openai.com/v1",
    "model": "gpt-4o-mini",
    "prompt": "Describe this image and identify anything notable about it.",
    "max_tokens": 500,
    "timeout_seconds": 60,
    "api_key_env": "OPENAI_API_KEY",
}


class Plugin:
    def __init__(self):
        self._active = True
        self._busy = False
        self._generation = None
        self._text_view = None
        self._panel = None
        self._config = dict(DEFAULTS)

    def activate(self, host):
        self._active = True
        self._generation = rsimgv.host.generation
        self._config = self._load_config()

    def deactivate(self):
        # A request already in flight cannot be cancelled safely. It is ignored
        # when it finishes because the generation and active flag no longer match.
        self._active = False
        self._text_view = None
        self._panel = None

    def invoke_action(self, action, context):
        if action != "analyze":
            return
        if self._busy:
            rsimgv.notify("An analysis is already running.", seconds=2)
            return

        try:
            info = rsimgv.host.call("view.info")
            screenshot = rsimgv.host.call("view.screenshot")
        except Exception as exc:
            rsimgv.notify(f"LLM Screenshot: {exc}", level="error", seconds=10)
            return

        data = screenshot.get("data")
        if not isinstance(data, str) or not data:
            rsimgv.notify(
                "LLM Screenshot: the viewer has no screenshot to send.",
                level="error",
                seconds=10,
            )
            return
        if len(data) > MAX_BASE64:
            rsimgv.notify(
                "LLM Screenshot: the screenshot is too large to send.",
                level="error",
                seconds=10,
            )
            return

        api_key = os.environ.get(self._config["api_key_env"], "").strip()
        if not api_key:
            rsimgv.notify(
                f"LLM Screenshot: set {self._config['api_key_env']} to an API key.",
                level="error",
                seconds=10,
            )
            return

        filename = info.get("filename") or "the current item"
        self._busy = True
        generation = self._generation
        threading.Thread(
            target=self._request,
            args=(generation, api_key, data, filename),
            daemon=True,
        ).start()

    def create_panel(self, panel_id, instance_id, container, context):
        if panel_id != "response":
            return

        view = Gtk.TextView()
        view.set_editable(False)
        view.set_cursor_visible(False)
        view.set_monospace(True)
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.set_margin_top(8)
        view.set_margin_bottom(8)
        view.set_margin_start(8)
        view.set_margin_end(8)

        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_min_content_height(120)
        scrolled.set_child(view)

        container.append(scrolled)
        self._text_view = view
        self._panel = (panel_id, instance_id)

    def panel_destroyed(self, panel_id, instance_id):
        if self._panel == (panel_id, instance_id):
            self._panel = None
            self._text_view = None

    # -- request ---------------------------------------------------------

    def _request(self, generation, api_key, data, filename):
        try:
            text = self._post(api_key, data)
            error = None
        except Exception as exc:
            text = None
            error = str(exc)

        # The generation check makes a late reply from an old reload harmless.
        rsimgv.ui.idle_add(
            lambda: self._finish(generation, filename, error, text)
        )

    def _post(self, api_key, data):
        body = {
            "model": self._config["model"],
            "max_tokens": self._config["max_tokens"],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": self._config["prompt"]},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": f"data:image/png;base64,{data}",
                            },
                        },
                    ],
                }
            ],
        }

        request = urllib.request.Request(
            f"{self._config['base_url'].rstrip('/')}/chat/completions",
            data=json.dumps(body).encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {api_key}",
            },
            method="POST",
        )

        try:
            with urllib.request.urlopen(
                request, timeout=self._config["timeout_seconds"]
            ) as response:
                text = response.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", "replace")
            try:
                message = json.loads(detail).get("error", {}).get("message", detail)
            except Exception:
                message = detail
            raise RuntimeError(f"HTTP {exc.code}: {str(message)[:300]}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"request failed: {exc.reason}") from exc

        value = json.loads(text)
        if isinstance(value, dict) and value.get("error"):
            message = value["error"].get("message", value["error"])
            raise RuntimeError(str(message)[:300])

        choices = value.get("choices") or []
        if not choices:
            raise RuntimeError("the response has no choices")

        content = choices[0].get("message", {}).get("content")
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts = [
                part.get("text", "")
                for part in content
                if isinstance(part, dict)
            ]
            return "\n".join(part for part in parts if part)
        raise RuntimeError("the response has no text")

    def _finish(self, generation, filename, error, text):
        if not self._active or generation != self._generation:
            return
        self._busy = False

        if error is not None:
            rsimgv.notify(f"LLM Screenshot: {error}", level="error", seconds=10)
            rsimgv.action_done({"error": error})
            return

        if self._text_view is not None:
            self._text_view.get_buffer().set_text(text or "")

        # Show the whole description over the window, where it can be read and dismissed.
        try:
            rsimgv.host.call(
                "messages.show",
                {
                    "text": text or "(the model returned an empty response)",
                    "persistent": True,
                    "dismissible": True,
                    "position": "bottom-center",
                },
            )
        except Exception as exc:
            rsimgv.notify(f"LLM Screenshot: {exc}", level="error", seconds=10)
        rsimgv.action_done({"result": text or ""})

    # -- configuration ---------------------------------------------------

    def _load_config(self):
        config = dict(DEFAULTS)
        config_path = rsimgv.host.config_path()
        path = os.path.join(os.path.dirname(config_path), "config.json")

        try:
            with open(path, "r", encoding="utf-8") as file:
                loaded = json.load(file)
            if isinstance(loaded, dict):
                config.update(
                    {key: value for key, value in loaded.items() if key in DEFAULTS}
                )
        except FileNotFoundError:
            pass
        except Exception as exc:
            rsimgv.notify(
                f"LLM Screenshot: cannot read {path}: {exc}",
                level="warning",
                seconds=8,
            )

        config["base_url"] = str(config["base_url"])
        config["model"] = str(config["model"])
        config["prompt"] = str(config["prompt"])
        config["api_key_env"] = str(config["api_key_env"])
        try:
            config["max_tokens"] = max(1, min(8192, int(config["max_tokens"])))
        except (TypeError, ValueError):
            config["max_tokens"] = DEFAULTS["max_tokens"]
        try:
            config["timeout_seconds"] = max(
                5, min(300, int(config["timeout_seconds"]))
            )
        except (TypeError, ValueError):
            config["timeout_seconds"] = DEFAULTS["timeout_seconds"]
        return config
