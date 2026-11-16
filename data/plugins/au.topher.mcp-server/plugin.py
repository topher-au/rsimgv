"""MCP Server: control rsimgv from MCP clients.

The plugin serves the Model Context Protocol (streamable HTTP transport, JSON
responses) on 127.0.0.1. Each tool maps onto a host method, the same ones the
IPC socket offers. HTTP requests are handled on worker threads; every host
call is queued to the GTK main thread with ``rsimgv.ui.idle_add``.
"""

import hmac
import ipaddress
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import rsimgv

PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
CALL_TIMEOUT = 130  # a little over the host's 120 s wait for plugin actions
MAX_BODY = 1_000_000
LOCAL_HOSTS = {"127.0.0.1", "localhost", "::1"}
DEFAULTS = {"port": 8765, "bind_address": "127.0.0.1", "token": "", "read_only": False}

# Tools that change state or reach arbitrary host methods; hidden when read_only is on.
WRITE_TOOLS = {"open_tab", "activate_tab", "close_tab", "select_file", "select_file_index", "navigate_to",
               "navigate_back", "navigate_forward", "navigate_up", "next_folder", "previous_folder",
               "next_media_folder", "previous_media_folder", "run_action", "show_message", "set_setting",
               "call_method"}


def _obj(properties=None, required=()):
    schema = {"type": "object", "properties": properties or {}}
    if required:
        schema["required"] = list(required)
    return schema


_STR = {"type": "string"}
_INT = {"type": "integer"}
_STRS = {"type": "array", "items": {"type": "string"}}

# name -> (description, input schema, host method, optional param builder)
TOOLS = {
    "view_info": ("Describe the picture or video in the viewer: filename, size, position, duration, fps.",
                  _obj(), "view.info"),
    "view_screenshot": ("Capture the picture or video frame the viewer shows, as an image.",
                        _obj(), "view.screenshot"),
    "window_info": ("Get the current folder, selected file and sidebar state.", _obj(), "window.info"),
    "list_tabs": ("List the open tabs.", _obj(), "tabs.list"),
    "open_tab": ("Open a folder in a new tab.",
                 _obj({"path": _STR, "activate": {"type": "boolean"}}, ["path"]), "tabs.open"),
    "activate_tab": ("Switch to a tab by index (from 0) or id.",
                     _obj({"index": _INT, "id": _STR}), "tabs.activate"),
    "close_tab": ("Close a tab by index.", _obj({"index": _INT}, ["index"]), "tabs.close"),
    "list_files": ("List the selected tab's items, or a folder's contents when path is given.",
                   _obj({"path": _STR}), "files.list"),
    "select_file": ("Select a file by name or path in the current tab.",
                    _obj({"file": _STR}, ["file"]), "files.select"),
    "select_file_index": ("Select a file by position; negative counts from the end.",
                          _obj({"index": _INT}, ["index"]), "files.select_index"),
    "selected_file": ("Get the selected file's name and index.", _obj(), "files.selected"),
    "navigate_to": ("Go to a folder.", _obj({"path": _STR}, ["path"]), "navigate.to"),
    "navigate_back": ("Go back in folder history.", _obj({"n": _INT}), "navigate.back"),
    "navigate_forward": ("Go forward in folder history.", _obj({"n": _INT}), "navigate.forward"),
    "navigate_up": ("Go up to the parent folder.", _obj({"n": _INT}), "navigate.up"),
    "next_folder": ("Go to the next folder in tree order.", _obj(), "navigate.next"),
    "previous_folder": ("Go to the previous folder in tree order.", _obj(), "navigate.previous"),
    "next_media_folder": ("Go to the next folder holding pictures or videos.", _obj(), "navigate.next_media"),
    "previous_media_folder": ("Go to the previous folder holding pictures or videos.",
                              _obj(), "navigate.previous_media"),
    "file_metadata": ("Get size, type and media details for files.",
                      _obj({"paths": _STRS}, ["paths"]), "metadata.get"),
    "list_bookmarks": ("List bookmarked folders.", _obj(), "bookmarks.list"),
    "list_actions": ("List every action, including keybound viewer actions such as zoom and playback.",
                     _obj(), "actions.list"),
    "run_action": ("Run an action by name (see list_actions). value and target are for actions that take them.",
                   _obj({"name": _STR, "value": {}, "target": {}}, ["name"]), "actions.run"),
    "show_message": ("Show a message over the rsimgv window.",
                     _obj({"text": _STR,
                           "level": {"type": "string", "enum": ["info", "success", "warning", "error", "plain"]},
                           "seconds": {"type": "number"}}, ["text"]), "messages.show"),
    "describe_settings": ("List application settings with type, description, default and value.",
                          _obj(), "settings.describe"),
    "get_setting": ("Read one application setting.", _obj({"key": _STR}, ["key"]), "settings.get"),
    "set_setting": ("Change an application setting; a null value resets it.",
                    _obj({"key": _STR, "value": {}}, ["key"]), "settings.set"),
    "call_method": ("Call any host method by name, as on the IPC socket (see src/services/remote.rs).",
                    _obj({"method": _STR, "params": {"type": "object"}}, ["method"]), None),
}


class _ToolError(Exception):
    pass


class Plugin:
    def __init__(self):
        self._server = None
        self._thread = None
        self._token = ""
        self._read_only = False

    def activate(self, host):
        config = self._load_config()
        self._token = str(config["token"])
        self._read_only = bool(config["read_only"])
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
                    "instructions": "Controls the rsimgv media viewer and file explorer.",
                }
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": [
                    {"name": name, "description": spec[0], "inputSchema": spec[1]}
                    for name, spec in TOOLS.items()
                    if not (self._read_only and name in WRITE_TOOLS)
                ]}
            elif method == "tools/call":
                result = self._call_tool(params.get("name"), params.get("arguments") or {})
            else:
                return {"jsonrpc": "2.0", "id": ident,
                        "error": {"code": -32601, "message": f"unknown method {method}"}}
        except Exception as exc:  # a failure outside the tool itself
            return {"jsonrpc": "2.0", "id": ident, "error": {"code": -32603, "message": str(exc)}}
        return {"jsonrpc": "2.0", "id": ident, "result": result}

    def _call_tool(self, name, arguments):
        spec = TOOLS.get(name)
        if spec is None or (self._read_only and name in WRITE_TOOLS):
            raise ValueError(f"unknown tool {name}")
        if not isinstance(arguments, dict):
            raise ValueError("arguments must be an object")
        if name == "call_method":
            method, params = arguments.get("method"), arguments.get("params") or {}
        else:
            method, params = spec[2], arguments
        try:
            result = self._host_call(method, params)
        except _ToolError as exc:
            return {"content": [{"type": "text", "text": str(exc)}], "isError": True}
        if method == "view.screenshot" and isinstance(result, dict) and result.get("data"):
            return {"content": [
                {"type": "image", "data": result["data"], "mimeType": "image/png"},
                {"type": "text", "text": f"{result.get('width')}x{result.get('height')} PNG"},
            ]}
        return {"content": [{"type": "text", "text": json.dumps(result, indent=2)}]}

    def _host_call(self, method, params):
        """Runs a host method on the GTK main thread and waits for its result."""
        if not isinstance(method, str) or not method:
            raise _ToolError("method must be a non-empty string")
        done = threading.Event()
        outcome = {}

        def run():
            try:
                outcome["result"] = rsimgv.host.call(method, params)
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
