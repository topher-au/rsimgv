"""Tests for the client against a stand-in server on each transport. The stand-in answers the JSON protocol the
way rsimgv does for the methods the client uses."""

import json
import socket
import socketserver
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from rsimgv_api import (
    Client,
    ConnectionFailed,
    HttpTransport,
    IpcError,
    NoSuchFile,
    NoSuchSetting,
    NoSuchTab,
    NotAFolder,
    TcpTransport,
    UnixTransport,
    UnknownAction,
)


def fake_answer(request: dict) -> dict:
    """Answers one request the way rsimgv's IPC does, for the methods these tests use."""
    method, params, ident = request["method"], request.get("params", {}), request["id"]

    def result(value):
        return {"id": ident, "result": value}

    def error(code, message):
        return {"id": ident, "error": {"code": code, "message": message}}

    if method == "actions.list":
        actions = [
            {"name": "win.toggle-sidebar", "label": "Toggle Sidebar", "description": "Show or hide the sidebar.",
             "scope": "global", "icon": "sidebar-show-symbolic", "defaults": ["f9"]},
            {"name": "view.close", "label": "Close Viewer", "description": "Close the viewer.",
             "scope": "view", "icon": "window-close-symbolic", "defaults": ["q", "escape"]},
        ]
        return result({"actions": actions})
    if method == "actions.run":
        if params.get("name") == "no.such":
            return error("unknown_action", "no action named no.such")
        return result({})
    if method == "window.info":
        return result({"current_path": "/a", "selected_file": "x.jpg", "show_sidebar": True})
    if method == "window.set_show_sidebar":
        return result({})
    if method == "tabs.current":
        return result({"id": 1, "index": 0})
    if method == "tabs.list":
        return result({"tabs": [{"id": 1, "index": 0, "path": "/a", "active": True}]})
    if method == "tabs.open":
        return result({"id": 2, "index": 1})
    if method in ("tabs.activate", "tabs.close"):
        if params.get("index") == 9 or params.get("id") == 9:
            return error("no_such_tab", "no tab at index 9")
        return result({})
    if method == "bookmarks.list":
        return result({"bookmarks": [{"name": "Trip", "path": "/a"}]})
    if method in ("bookmarks.add", "bookmarks.remove"):
        return result({})
    if params.get("tab") == 99:
        return error("no_such_tab", "no tab 99")
    if method == "tabs.files":
        return result({"files": [{"name": "x.jpg", "path": "/a/x.jpg", "is_dir": False}]})
    if method == "tabs.select":
        if params.get("file") == "nope":
            return error("no_such_file", "no item nope")
        return result({"selected": params["file"]})
    if method == "navigate.back":
        return result({"moved": params.get("n", 1)})
    if method == "navigate.to":
        if params.get("path") == "/bad":
            return error("not_a_folder", "/bad is not a folder")
        return result({})
    if method == "files.list":
        listing = {"x.jpg": {"path": "/a/x.jpg", "is_dir": False, "size": 3, "modified": 1.0}}
        return result({"files": listing})
    if method == "files.select":
        if params.get("file") == "nope":
            return error("no_such_file", "no item nope")
        return result({"selected": params["file"]})
    if method == "files.selected":
        return result({"name": "x.jpg", "index": 0})
    if method == "files.select_index":
        if params.get("index") == 9:
            return error("no_such_file", "no item at index 9")
        return result({"selected": "x.jpg"})
    if method == "view.screenshot":
        # "PNG!" in base64, standing in for an image file.
        return result({"format": "png", "width": 2, "height": 1, "data": "UE5HIQ=="})
    if method == "metadata.get":
        entries = []
        for path in params["paths"]:
            if path == "/nope":
                entries.append({"path": path, "error": "no_such_file"})
                continue
            entries.append({
                "path": path,
                "name": path.rsplit("/", 1)[-1],
                "is_dir": False,
                "size": 3,
                "modified": 1.0,
                "mime_type": "image/jpeg",
                "media": {
                    "width": 2, "height": 1, "duration": None, "frame_rate": None,
                    "codec": "jpeg", "audio_codec": None, "bit_rate": None,
                },
                "media_pending": False,
            })
        return result({"metadata": entries})
    if method == "files.state":
        if params.get("path") == "/nope":
            return error("file_error", "could not read the state")
        return result({"path": params["path"], "hash": "abc", "values": {"position": 93.5, "speed": 1.25}})
    if method in ("files.set_state", "files.state.set"):
        return result({"path": params["path"], "hash": "abc", "values": params["values"]})
    if method in ("files.reset_state", "files.state.reset"):
        return result({"path": params["path"], "hash": "abc", "values": {}})
    if method == "settings.list":
        return result({"settings": {"browse.item-size": 128, "browse.show-hidden": False}})
    if method == "settings.get":
        values = {"browse.item-size": 128, "browse.show-hidden": False}
        key = params.get("key")
        if key not in values:
            return error("no_such_setting", f"no setting named {key}")
        return result({"key": key, "value": values[key]})
    if method == "settings.set":
        value = 128 if params.get("value") is None else params["value"]
        return result({"key": params["key"], "value": value})
    if method == "settings.update":
        return result({"settings": params["values"]})
    if method == "settings.reset":
        return result({})
    if method == "settings.describe":
        return result({"settings": [
            {
                "key": "browse.item-size",
                "type": "i",
                "description": "Size in pixels of each browse item",
                "default": 128,
                "value": 128,
            },
        ]})
    return error("unknown_method", method)


class _LineHandler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            if line.strip():
                self.wfile.write(json.dumps(fake_answer(json.loads(line))).encode() + b"\n")


class _HttpHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.headers.get("Content-Type") != "application/json" or self.path != "/ipc":
            self.send_error(415)
            return
        body = fake_answer(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


def _serve(server):
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class TransportTests(unittest.TestCase):
    """The same checks run over each transport, so each one is held to the same protocol."""

    def _check_all(self, client: Client) -> None:
        self.assertEqual(client.list_actions(), ["win.toggle-sidebar", "view.close"])
        described = client.describe_actions()
        self.assertEqual(described[0].description, "Show or hide the sidebar.")
        self.assertEqual(described[1].defaults, ("q", "escape"))
        client.action.win.toggle_sidebar()
        self.assertIn("toggle_sidebar", dir(client.action.win))
        self.assertIn("win", dir(client.action))
        with self.assertRaises(UnknownAction) as caught:
            client.action("no.such")
        self.assertEqual(caught.exception.name, "no.such")

        self.assertEqual(client.current_path, "/a")
        self.assertEqual(client.selected_file, "x.jpg")
        client.selected_file = "x.jpg"
        self.assertTrue(client.show_sidebar)
        client.show_sidebar = False

        self.assertEqual(client.tabs.list(), {1: "/a"})
        self.assertEqual(client.tabs.active_id, 1)
        client.tabs.active_id = 1
        with self.assertRaises(NoSuchTab):
            client.tabs.active_id = 9
        self.assertEqual(client.tabs.new("/a"), 1)
        client.tabs.activate(0)
        client.tabs.close(0)
        with self.assertRaises(NoSuchTab):
            client.tabs.activate(9)

        self.assertEqual(client.bookmarks.list(), {"/a": "Trip"})
        self.assertEqual(client.action.win.toggle_sidebar.__doc__, "Show or hide the sidebar.")
        client.bookmarks.add("/a", "Trip")
        client.bookmarks.add("/a")
        client.bookmarks.remove("/a")

        self.assertEqual(client.files.list()["x.jpg"].size, 3)
        self.assertEqual(client.files.select(Path("/a/x.jpg")), "x.jpg")
        self.assertEqual(client.files.select_index(-1), "x.jpg")
        self.assertEqual(client.files.selected, "x.jpg")
        self.assertEqual(client.files.selected_index, 0)
        client.files.selected = "x.jpg"
        client.files.selected_index = -1
        with self.assertRaises(NoSuchFile):
            client.files.select("nope")
        with self.assertRaises(NoSuchFile):
            client.files.select_index(9)
        self.assertEqual(client.navigate.back(3), 3)
        with self.assertRaises(NotAFolder):
            client.navigate.to("/bad")

        self.assertEqual(client.metadata.one("/a/x.jpg").media.width, 2)
        metadata = client.metadata.get("/a/x.jpg", "/nope")
        self.assertEqual(metadata[0].mime_type, "image/jpeg")
        self.assertFalse(metadata[0].media_pending)
        self.assertEqual(metadata[1].error, "no_such_file")
        self.assertFalse(metadata[1].exists)
        self.assertEqual(client.files.metadata("/a/x.jpg")[0].size, 3)

        state = client.state.get("/a/x.mp4")
        self.assertEqual((state.hash, state["position"], state["speed"]), ("abc", 93.5, 1.25))
        self.assertEqual(client.files.state("/a/x.mp4").path, "/a/x.mp4")
        state = client.state.update("/a/x.mp4", position=12.0, speed=1.5)
        self.assertEqual(state.values, {"position": 12.0, "speed": 1.5})
        self.assertEqual(client.state.set("/a/x.mp4", "volume", 0.5)["volume"], 0.5)
        self.assertEqual(client.state.remove("/a/x.mp4", "speed").values, {})
        with self.assertRaises(IpcError):
            client.state.get("/nope")

        self.assertEqual(client.settings.get("browse.item-size"), 128)
        self.assertFalse(client.settings["browse.show-hidden"])
        self.assertEqual(client.settings.get("no.such", 7), 7)
        with self.assertRaises(NoSuchSetting):
            client.settings.get("no.such")
        self.assertEqual(client.settings.set("browse.item-size", 160), 160)
        self.assertEqual(client.settings.update({"browse.show-hidden": True}), {"browse.show-hidden": True})
        client.settings.reset("browse.item-size")
        self.assertEqual(client.settings.describe()[0].default, 128)
        self.assertIn("browse.item-size", client.settings)

        shot = client.screenshot()
        self.assertEqual((shot.format, shot.width, shot.height, shot.data), ("png", 2, 1, b"PNG!"))

    def test_unix_socket(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "rsimgv.sock")
            server = _serve(socketserver.ThreadingUnixStreamServer(path, _LineHandler))
            try:
                self._check_all(Client.from_unix(path, timeout=5))
            finally:
                server.shutdown()
                server.server_close()

    def test_tcp_socket(self):
        server = _serve(socketserver.ThreadingTCPServer(("127.0.0.1", 0), _LineHandler))
        try:
            self._check_all(Client.from_tcp(server.server_address[1], timeout=5))
        finally:
            server.shutdown()
            server.server_close()

    def test_http(self):
        server = _serve(ThreadingHTTPServer(("127.0.0.1", 0), _HttpHandler))
        try:
            self._check_all(Client.from_http(server.server_address[1], timeout=5))
        finally:
            server.shutdown()
            server.server_close()

    def test_the_default_transport_is_the_unix_socket(self):
        self.assertIsInstance(Client()._transport, UnixTransport)

    def test_transports_can_be_passed_in_directly(self):
        server = _serve(ThreadingHTTPServer(("127.0.0.1", 0), _HttpHandler))
        try:
            self._check_all(Client(HttpTransport(server.server_address[1], timeout=5)))
            self.assertIsInstance(Client(TcpTransport(1)).__dict__["_transport"], TcpTransport)
        finally:
            server.shutdown()
            server.server_close()

    def test_nothing_listening_is_a_connection_failure(self):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        with self.assertRaises(ConnectionFailed):
            Client.from_tcp(port, timeout=2).list_actions()


if __name__ == "__main__":
    unittest.main()
