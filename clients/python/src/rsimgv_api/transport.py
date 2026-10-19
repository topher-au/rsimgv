"""How requests reach rsimgv: a Unix socket, a TCP socket, or HTTP. All three carry the same JSON requests and
answers; see the IPC module in rsimgv for the protocol."""

from __future__ import annotations

import json
import os
import socket
import urllib.error
import urllib.request
from pathlib import Path
from typing import Protocol


def default_socket_path() -> Path:
    """The Unix socket rsimgv serves: ``rsimgv.sock`` in the runtime directory, which falls back to the cache
    directory as GLib does."""
    runtime = os.environ.get("XDG_RUNTIME_DIR") or os.environ.get("XDG_CACHE_HOME") or str(Path.home() / ".cache")
    return Path(runtime) / "rsimgv.sock"


class ConnectionFailed(Exception):
    """rsimgv could not be reached, or closed the connection without answering."""


class RequestRefused(Exception):
    """rsimgv answered over HTTP with an error status, so the request never reached the protocol."""


class Transport(Protocol):
    def exchange(self, request: dict) -> dict:
        """Sends one request and returns the response object."""


def _read_line(sock: socket.socket) -> bytes:
    buffer = bytearray()
    while b"\n" not in buffer:
        chunk = sock.recv(65536)
        if not chunk:
            break
        buffer.extend(chunk)
    return bytes(buffer).split(b"\n", 1)[0]


class _SocketTransport:
    """Shared by the Unix and TCP socket transports: one connection per request, one line each way."""

    def __init__(self, timeout: float) -> None:
        self.timeout = timeout

    def _connect(self) -> socket.socket:
        raise NotImplementedError

    def exchange(self, request: dict) -> dict:
        try:
            sock = self._connect()
            with sock:
                sock.settimeout(self.timeout)
                sock.sendall(json.dumps(request).encode() + b"\n")
                line = _read_line(sock)
        except OSError as error:
            raise ConnectionFailed(f"could not reach rsimgv: {error}") from error
        if not line:
            raise ConnectionFailed("rsimgv closed the connection without answering")
        return json.loads(line)


class UnixTransport(_SocketTransport):
    """Talks to rsimgv over its Unix socket. With no path, uses the socket in the runtime directory."""

    def __init__(self, path: str | Path | None = None, timeout: float = 10.0) -> None:
        super().__init__(timeout)
        self.path = str(default_socket_path() if path is None else path)

    def _connect(self) -> socket.socket:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(self.path)
        except OSError:
            sock.close()
            raise
        return sock


class TcpTransport(_SocketTransport):
    """Talks to rsimgv over a localhost TCP port."""

    def __init__(self, port: int, host: str = "127.0.0.1", timeout: float = 10.0) -> None:
        super().__init__(timeout)
        self.address = (host, port)

    def _connect(self) -> socket.socket:
        return socket.create_connection(self.address, timeout=self.timeout)


class HttpTransport:
    """Talks to rsimgv over HTTP, posting each request as JSON to ``/ipc``."""

    def __init__(self, port: int, host: str = "127.0.0.1", timeout: float = 10.0) -> None:
        self.url = f"http://{host}:{port}/ipc"
        self.timeout = timeout

    def exchange(self, request: dict) -> dict:
        http_request = urllib.request.Request(
            self.url,
            data=json.dumps(request).encode(),
            method="POST",
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(http_request, timeout=self.timeout) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as error:
            message = error.read().decode("utf-8", "replace").strip()
            raise RequestRefused(f"rsimgv answered HTTP {error.code}: {message}") from error
        except (urllib.error.URLError, OSError) as error:
            raise ConnectionFailed(f"could not reach rsimgv at {self.url}: {error}") from error
