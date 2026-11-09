"""Client for rsimgv's JSON IPC, over a Unix socket, a TCP socket, or HTTP.

    from rsimgv_api import Client

    client = Client()                      # the Unix socket in the runtime directory
    client = Client(TcpTransport(7880))    # or another transport
    client.action.win.toggle_sidebar()     # runs the action win.toggle-sidebar
    client.show_sidebar = False
    dir(client.action)                     # the action groups
    dir(client.action.win)                 # the actions in the win group
    client.tabs.new("/home/me/Pictures", activate=True)
    for name, info in client.files.list().items():
        print(name, info.size)
    client.files.select("holiday.jpg")
    client.navigate.up()
    client.action.video.seek(-30)          # an action that takes a value
    client.screenshot().save("frame.png")  # what the viewer shows

The application settings, the metadata of files, and the state kept for each file are available too:

    client.settings["browse.item-size"] = 160
    client.settings.update({"browse.show-hidden": True, "browse.media-only": False})
    client.settings.reset("browse.item-size")

    metadata = client.metadata.one("/home/me/Pictures/holiday.jpg")
    print(metadata.media.width, metadata.media.height)
    client.metadata.get("a.jpg", "b.mp4")     # one entry per path, in the order asked

    state = client.state.get("/home/me/Videos/clip.mp4")
    print(state.values["position"])           # where the video was left
    client.state.update("/home/me/Videos/clip.mp4", position=93.5, speed=1.25)
    client.state.remove("/home/me/Videos/clip.mp4", "speed")

Every value rsimgv answers with is a Pydantic model, so it can be turned into JSON:

    print(metadata.to_json())                 # or metadata.model_dump_json()
    print(state.to_dict())                    # just the saved values
    metadata = type(metadata).from_json(text)  # read a value back
"""

from __future__ import annotations

import base64
import itertools
import json
import os
import socket
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import TYPE_CHECKING, Any, Protocol, TypeVar, Union

from pydantic import BaseModel, ConfigDict, Field

if TYPE_CHECKING:
    import PIL.Image

DEFAULT_TCP_HOST = "127.0.0.1"

#: A tab id, or "current" for the selected tab.
TabRef = Union[int, str]

#: Tells a lookup with no default from one whose default is None.
_MISSING = object()

_ModelT = TypeVar("_ModelT", bound="RsimgvModel")


class RsimgvModel(BaseModel):
    """Base for the values rsimgv answers with. A value does not change once made, and it serializes to JSON.

    ``model_dump()`` gives one as a dict, and ``model_dump_json()`` or :meth:`to_json` as a JSON string.
    """

    model_config = ConfigDict(
        frozen=True,
        # Raw image bytes in a screenshot become base64 text in JSON, and back again in :meth:`from_json`.
        ser_json_bytes="base64",
        val_json_bytes="base64",
    )

    def to_json(self, *, indent: int | None = None) -> str:
        """The value as a JSON string."""
        return self.model_dump_json(indent=indent)

    @classmethod
    def from_json(cls: type[_ModelT], data: str | bytes) -> _ModelT:
        """The value :meth:`to_json` gave back from its JSON string."""
        return cls.model_validate_json(data)


class RsimgvError(Exception):
    """Base class for every error this library raises from an answer rsimgv gave."""


class IpcError(RsimgvError):
    """rsimgv answered with an error. ``code`` is the short code it gave; ``message`` says more."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class UnknownAction(IpcError):
    name: str = ""


class ActionNotRun(IpcError):
    """The action exists, but the window did not run it for what is shown."""


class NoSuchTab(IpcError):
    pass


class NoSuchFile(IpcError):
    pass


class NotAFolder(IpcError):
    pass


class NoSuchSetting(IpcError):
    """The application has no setting with that name."""


class NoMedia(IpcError):
    """The viewer is not showing a picture or a video frame to capture."""


_ERRORS: dict[str, type[IpcError]] = {
    "unknown_action": UnknownAction,
    "not_run": ActionNotRun,
    "no_such_tab": NoSuchTab,
    "no_such_file": NoSuchFile,
    "not_a_folder": NotAFolder,
    "no_such_setting": NoSuchSetting,
    "no_media": NoMedia,
}


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

    def __init__(self, port: int, host: str = DEFAULT_TCP_HOST, timeout: float = 10.0) -> None:
        super().__init__(timeout)
        self.address = (host, port)

    def _connect(self) -> socket.socket:
        return socket.create_connection(self.address, timeout=self.timeout)


class HttpTransport:
    """Talks to rsimgv over HTTP, posting each request as JSON to ``/ipc``."""

    def __init__(self, port: int, host: str = DEFAULT_TCP_HOST, timeout: float = 10.0) -> None:
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


class Screenshot(RsimgvModel):
    """The media the viewer showed, at its own size: ``data`` is the image file's bytes, in ``format``."""

    format: str
    width: int
    height: int
    # Left out of the repr, as an image is too long to print.
    data: bytes = Field(repr=False)

    def to_image(self, max_size: int | tuple[int, int] | None = None) -> "PIL.Image.Image":
        """The image as a Pillow image. With ``max_size``, a number or a ``(width, height)``, it is scaled down to fit
        within that size, keeping its shape; it is never enlarged. Needs Pillow installed."""
        import io

        from PIL import Image

        image = Image.open(io.BytesIO(self.data))
        image.load()
        if max_size is not None:
            bounds = (max_size, max_size) if isinstance(max_size, int) else max_size
            image.thumbnail(bounds)
        return image

    def save(self, path: str | os.PathLike[str]) -> Path:
        """Writes the image to ``path`` and returns it."""
        path = Path(path)
        path.write_bytes(self.data)
        return path


class ActionInfo(RsimgvModel):
    """What rsimgv says about one action."""

    name: str
    label: str
    description: str
    scope: str
    icon: str
    defaults: tuple[str, ...]


class FileInfo(RsimgvModel):
    """What rsimgv says about one item in a folder. ``size`` is in bytes, for a file; ``modified`` is in seconds
    since the Unix epoch. Either is None when rsimgv could not read it."""

    name: str
    path: str
    is_dir: bool
    size: int | None
    modified: float | None


class MediaMetadata(RsimgvModel):
    """What rsimgv read from a picture or a video. ``duration`` is in seconds, ``frame_rate`` in frames per second,
    and ``bit_rate`` in bits per second. A field rsimgv has not read yet is None."""

    width: int | None = None
    height: int | None = None
    duration: float | None = None
    frame_rate: float | None = None
    codec: str | None = None
    audio_codec: str | None = None
    bit_rate: int | None = None


#: A shorter name for :class:`MediaMetadata`.
MediaInfo = MediaMetadata


class FileMetadata(RsimgvModel):
    """The metadata of one path. ``media`` is None for a folder or a file that is not a picture or video, and also
    while rsimgv is still reading a picture or video's metadata; ``media_pending`` says which. A path that does not
    exist has ``error`` set to ``"no_such_file"``."""

    path: str
    name: str | None
    is_dir: bool
    size: int | None
    modified: float | None
    mime_type: str | None = None
    media: MediaMetadata | None = None
    media_pending: bool = False
    error: str | None = None

    @property
    def exists(self) -> bool:
        """Whether the path exists, as rsimgv found it."""
        return self.error is None


class FileState(RsimgvModel):
    """The state rsimgv keeps for one file, such as where a video was left. ``values`` holds the keys as they are
    saved, which is empty for a file with no state yet. ``hash`` is the content hash the state is kept under, so it
    follows a file that is moved or renamed."""

    path: str
    hash: str | None
    values: dict[str, Any] = Field(default_factory=dict)

    def __getitem__(self, key: str) -> Any:
        return self.values[key]

    def __contains__(self, key: str) -> bool:
        return key in self.values

    def __iter__(self):
        return iter(self.values)

    def __len__(self) -> int:
        return len(self.values)

    def get(self, key: str, default: Any = None) -> Any:
        """The value of ``key``, or ``default`` if it is not set."""
        return self.values.get(key, default)

    def keys(self):
        return self.values.keys()

    def items(self):
        return self.values.items()

    def to_dict(self) -> dict[str, Any]:
        """The values as a dict."""
        return dict(self.values)


class SettingInfo(RsimgvModel):
    """What rsimgv says about one application setting, including its schema type and default, and its current value."""

    key: str
    type: str
    description: str
    default: Any
    value: Any

    @property
    def kind(self) -> str:
        """The setting's GVariant type, such as ``b`` or ``as``."""
        return self.type


def _paths(arguments: tuple[Any, ...]) -> list[Any]:
    """The paths in a call: either separate paths, or a single list or tuple of them."""
    if len(arguments) == 1 and isinstance(arguments[0], (list, tuple, set, frozenset)):
        return list(arguments[0])
    return list(arguments)


class TabsNamespace:
    """The window's tabs, by position: index 0 is the first tab."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    @property
    def active_id(self) -> int | None:
        """The id of the selected tab, or None when no tab is open. Setting it selects that tab."""
        return self._client.call("tabs.current")["id"]

    @active_id.setter
    def active_id(self, tab_id: int) -> None:
        self._client.call("tabs.activate", id=tab_id)

    def list(self) -> dict[int, str | None]:
        """The open tabs, as a dict from each tab's id to its folder's path. Tabs are in window order."""
        return {tab["id"]: tab["path"] for tab in self._client.call("tabs.list")["tabs"]}

    def new(self, path: str | os.PathLike[str], activate: bool = False) -> int:
        """Opens a folder in a new tab, and returns the new tab's position."""
        return int(self._client.call("tabs.open", path=os.fspath(path), activate=activate)["index"])

    def close(self, index: int) -> None:
        self._client.call("tabs.close", index=index)

    def activate(self, index: int) -> None:
        self._client.call("tabs.activate", index=index)


class BookmarksNamespace:
    """The folders bookmarked in the sidebar."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def add(self, path: str | os.PathLike[str], name: str | None = None) -> None:
        """Bookmarks a folder under ``name``, or under the folder's own name. Bookmarking it again renames it."""
        params: dict[str, Any] = {"path": os.fspath(path)}
        if name is not None:
            params["name"] = name
        self._client.call("bookmarks.add", **params)

    def list(self) -> dict[str, str]:
        """The bookmarks, as a dict from each folder's path to its bookmark name."""
        return {bookmark["path"]: bookmark["name"] for bookmark in self._client.call("bookmarks.list")["bookmarks"]}

    def remove(self, path: str | os.PathLike[str]) -> None:
        self._client.call("bookmarks.remove", path=os.fspath(path))


class NavigateNamespace:
    """Moving the window between folders."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def back(self, n: int = 1) -> int:
        """Goes back ``n`` folders in the history. Returns how many steps were taken."""
        return int(self._client.call("navigate.back", n=n)["moved"])

    def forward(self, n: int = 1) -> int:
        """Goes forward ``n`` folders in the history. Returns how many steps were taken."""
        return int(self._client.call("navigate.forward", n=n)["moved"])

    def up(self, n: int = 1) -> int:
        """Goes up ``n`` folders, stopping at the root. Returns how many levels were climbed."""
        return int(self._client.call("navigate.up", n=n)["moved"])

    def next(self) -> str | None:
        """Shows the next folder in depth-first order of the folder tree: a folder's first subfolder, then its
        following siblings, then its parent's following siblings. Returns the folder shown, or None at the end, in
        which case the window stays where it is."""
        return self._client.call("navigate.next")["path"]

    def previous(self) -> str | None:
        """Shows the previous folder in depth-first order of the folder tree. Returns the folder shown, or None at
        the start, in which case the window stays where it is."""
        return self._client.call("navigate.previous")["path"]

    def next_media(self) -> str | None:
        """Shows the next folder with a picture or video, and selects its first one, or shows it in the viewer if the
        viewer is open. Returns the folder moved to, or None at the end."""
        return self._client.call("navigate.next_media")["path"]

    def previous_media(self) -> str | None:
        """Shows the previous folder with a picture or video, and selects its last one, or shows its first one in the
        viewer if the viewer is open. Returns the folder moved to, or None at the start."""
        return self._client.call("navigate.previous_media")["path"]

    def to(self, path: str | os.PathLike[str]) -> None:
        """Shows the folder at ``path``."""
        self._client.call("navigate.to", path=os.fspath(path))


def _file_metadata(item: dict) -> FileMetadata:
    """One entry of a ``metadata.get`` answer."""
    media = item.get("media")
    return FileMetadata(
        path=item["path"],
        name=item.get("name"),
        is_dir=bool(item.get("is_dir")),
        size=item.get("size"),
        modified=item.get("modified"),
        mime_type=item.get("mime_type"),
        media=None
        if media is None
        else MediaMetadata(
            width=media.get("width"),
            height=media.get("height"),
            duration=media.get("duration"),
            frame_rate=media.get("frame_rate"),
            codec=media.get("codec"),
            audio_codec=media.get("audio_codec"),
            bit_rate=media.get("bit_rate"),
        ),
        media_pending=bool(item.get("media_pending")),
        error=item.get("error"),
    )


def _file_state(result: dict) -> FileState:
    """A ``files.state`` answer."""
    return FileState(path=result["path"], hash=result.get("hash"), values=dict(result.get("values", {})))


class MetadataNamespace:
    """What rsimgv knows about files: their name, size and modification time, their MIME type, and what it read
    from a picture or video."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def get(self, *paths: str | os.PathLike[str]) -> list[FileMetadata]:
        """The metadata of each path, in the order given. A path may also be given as one list or tuple. A path that
        does not exist has its ``error`` set, rather than raising. A picture or video whose metadata rsimgv has not
        read yet is queued to be read, so its ``media`` is None and ``media_pending`` is True for now."""
        wanted = _paths(paths)
        if not wanted:
            return []
        result = self._client.call("metadata.get", paths=[os.fspath(path) for path in wanted])
        return [_file_metadata(item) for item in result["metadata"]]

    def one(self, path: str | os.PathLike[str]) -> FileMetadata:
        """The metadata of one path."""
        return self.get(path)[0]

    def wait(
        self,
        *paths: str | os.PathLike[str],
        timeout: float = 5.0,
        interval: float = 0.2,
    ) -> list[FileMetadata]:
        """The metadata of each path, asking again while rsimgv is still reading any picture or video's metadata,
        until ``timeout`` seconds have passed. Returns what is known at that point."""
        deadline = time.monotonic() + timeout
        while True:
            items = self.get(*paths)
            if not any(item.media_pending for item in items) or time.monotonic() >= deadline:
                return items
            time.sleep(min(interval, max(0.0, deadline - time.monotonic())))

    def __call__(self, *paths: str | os.PathLike[str]) -> list[FileMetadata]:
        return self.get(*paths)


class StateNamespace:
    """The state rsimgv keeps for each file, by its contents rather than its path, so it follows a file that is
    moved or renamed. A video keeps its ``position`` in seconds, ``speed``, ``volume`` and ``muted``, and ``aspect``;
    a picture or video keeps ``rotation`` in degrees, ``mirrored``, and a picture also ``fit``, ``zoom``, ``pan-x``
    and ``pan-y``. A file that is locked to a crop keeps ``crop-locked`` and ``crop-width``, ``crop-height``,
    ``crop-left`` and ``crop-top``."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def get(self, path: str | os.PathLike[str]) -> FileState:
        """The state saved for the file at ``path``."""
        return _file_state(self._client.call("files.state", path=os.fspath(path)))

    def update(
        self,
        path: str | os.PathLike[str],
        values: dict[str, Any] | None = None,
        **changes: Any,
    ) -> FileState:
        """Sets each key in ``values`` and ``changes`` for the file at ``path``, and returns its state afterwards. A
        value of None removes the key."""
        merged: dict[str, Any] = dict(values or {})
        merged.update(changes)
        result = self._client.call("files.set_state", path=os.fspath(path), values=merged)
        return _file_state(result)

    def set(self, path: str | os.PathLike[str], key: str, value: Any) -> FileState:
        """Sets one key of the state of the file at ``path``. A value of None removes it."""
        return self.update(path, {key: value})

    def remove(self, path: str | os.PathLike[str], *keys: str) -> FileState:
        """Removes each key from the state of the file at ``path``. With no keys, removes the whole state."""
        wanted = _paths(keys)
        if not wanted:
            return self.clear(path)
        result = self._client.call("files.reset_state", path=os.fspath(path), keys=[str(key) for key in wanted])
        return _file_state(result)

    def clear(self, path: str | os.PathLike[str]) -> FileState:
        """Removes all the state saved for the file at ``path``."""
        return _file_state(self._client.call("files.reset_state", path=os.fspath(path)))

    def __getitem__(self, path: str | os.PathLike[str]) -> FileState:
        return self.get(path)

    def __call__(self, path: str | os.PathLike[str]) -> FileState:
        return self.get(path)


class SettingsNamespace:
    """The application's settings, by their dotted names, such as ``browse.item-size`` or ``viewer.status-bar.shown``.
    Reading and writing them changes the running application as well as the settings file. A value can also be
    reached as an item: ``client.settings["browse.item-size"]``."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def list(self) -> dict[str, Any]:
        """Every setting rsimgv has, as a dict from its dotted name to its current value."""
        return dict(self._client.call("settings.list")["settings"])

    def all(self) -> dict[str, Any]:
        """Every setting rsimgv has, as a dict from its dotted name to its current value."""
        return self.list()

    def get(self, key: str, default: Any = _MISSING) -> Any:
        """The current value of the setting ``key``. With ``default``, it is returned when rsimgv has no such
        setting."""
        try:
            return self._client.call("settings.get", key=key)["value"]
        except NoSuchSetting:
            if default is not _MISSING:
                return default
            raise

    def set(self, key: str, value: Any) -> Any:
        """Sets the setting ``key`` to ``value`` and returns the value as rsimgv saved it. A value of None resets it
        to its schema default."""
        return self._client.call("settings.set", key=key, value=value).get("value", value)

    def update(self, values: dict[str, Any] | None = None, **changes: Any) -> dict[str, Any]:
        """Sets each setting in ``values`` and ``changes``, and returns the values saved. A value of None resets that
        setting to its schema default."""
        merged: dict[str, Any] = dict(values or {})
        merged.update(changes)
        return dict(self._client.call("settings.update", values=merged).get("settings", {}))

    def reset(self, *keys: str) -> None:
        """Resets each setting named to its schema default. With no keys, resets every setting."""
        wanted = _paths(keys)
        params: dict[str, Any] = {} if not wanted else {"keys": [str(key) for key in wanted]}
        self._client.call("settings.reset", **params)

    def describe(self) -> list[SettingInfo]:
        """Every setting, with its schema type, description and default, and its current value."""
        return [
            SettingInfo(
                key=item["key"],
                type=item.get("type", ""),
                description=item.get("description") or "",
                default=item.get("default"),
                value=item.get("value"),
            )
            for item in self._client.call("settings.describe")["settings"]
        ]

    def keys(self) -> list[str]:
        """The dotted name of every setting."""
        return list(self.list())

    def items(self):
        """Every setting's dotted name and current value."""
        return self.list().items()

    def __getitem__(self, key: str) -> Any:
        return self.get(key)

    def __setitem__(self, key: str, value: Any) -> None:
        self.set(key, value)

    def __delitem__(self, key: str) -> None:
        self.reset(key)

    def __contains__(self, key: str) -> bool:
        return key in self.list()

    def __iter__(self):
        return iter(self.list())

    def __len__(self) -> int:
        return len(self.list())


class FilesNamespace:
    """The items in folders, and selecting them in the selected tab."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def list(self, path: str | os.PathLike[str] | None = None) -> dict[str, FileInfo]:
        """The items in a folder, as a dict from each name to its info. With no path, the items the selected tab
        shows, in the order it shows them. With a path, that folder's items from disk, sorted by name."""
        params: dict[str, Any] = {} if path is None else {"path": os.fspath(path)}
        return {
            name: FileInfo(
                name=name,
                path=info["path"],
                is_dir=info["is_dir"],
                size=info["size"],
                modified=info["modified"],
            )
            for name, info in self._client.call("files.list", **params)["files"].items()
        }

    def metadata(self, *paths: str | os.PathLike[str]) -> list[FileMetadata]:
        """The metadata of each path, as :meth:`MetadataNamespace.get` gives it."""
        return self._client.metadata.get(*paths)

    def state(self, path: str | os.PathLike[str]) -> FileState:
        """The state saved for the file at ``path``."""
        return self._client.state.get(path)

    def get_state(self, path: str | os.PathLike[str]) -> FileState:
        """The state saved for the file at ``path``."""
        return self.state(path)

    def set_state(self, path: str | os.PathLike[str], values: dict[str, Any] | None = None, **changes: Any) -> FileState:
        """Sets the state of the file at ``path``, as :meth:`StateNamespace.update` does."""
        return self._client.state.update(path, values, **changes)

    def reset_state(self, path: str | os.PathLike[str], *keys: str) -> FileState:
        """Removes keys from the state of the file at ``path``, as :meth:`StateNamespace.remove` does."""
        return self._client.state.remove(path, *keys)

    def remove_state(self, path: str | os.PathLike[str], *keys: str) -> FileState:
        """Removes keys from the state of the file at ``path``, as :meth:`StateNamespace.remove` does."""
        return self.reset_state(path, *keys)

    @property
    def selected(self) -> str | None:
        """The name of the item selected in the selected tab, or None if nothing is selected. Setting it selects
        that item, by its name or its path."""
        return self._client.call("files.selected")["name"]

    @selected.setter
    def selected(self, filename: str | os.PathLike[str]) -> None:
        self.select(filename)

    @property
    def selected_index(self) -> int | None:
        """The position of the item selected in the selected tab, or None if nothing is selected. Setting it selects
        the item at that position; negative positions count from the end."""
        return self._client.call("files.selected")["index"]

    @selected_index.setter
    def selected_index(self, index: int) -> None:
        self.select_index(index)

    def select(self, filename: str | os.PathLike[str]) -> str:
        """Selects an item in the selected tab, by its name or its path, and returns the name selected."""
        return str(self._client.call("files.select", file=os.fspath(filename))["selected"])

    def select_index(self, index: int) -> str:
        """Selects the item at ``index`` in the selected tab. Negative indexes count from the end. Returns the
        name selected."""
        return str(self._client.call("files.select_index", index=index)["selected"])


class ActionNamespace:
    """``client.action.win.toggle_sidebar()`` runs the action ``win.toggle-sidebar``. Underscores in attribute
    names stand for dashes. Calling the namespace itself runs a named action: ``client.action("view.close")``."""

    def __init__(self, client: "Client") -> None:
        self._client = client

    def __call__(self, name: str, value: float | None = None) -> None:
        self._client.run_action(name, value)

    def __dir__(self) -> list[str]:
        """The action groups rsimgv has, such as ``win``, ``browse`` and ``video``. Asks rsimgv for them; if it
        cannot be reached, only the names every namespace has are listed."""
        names = set(super().__dir__())
        try:
            names.update(name.split(".", 1)[0] for name in self._client.list_actions())
        except (RsimgvError, ConnectionFailed):
            pass
        return sorted(names)

    def __getattr__(self, group: str) -> "ActionGroup":
        if group.startswith("_"):
            raise AttributeError(group)
        return ActionGroup(self._client, group)


class ActionGroup:
    def __init__(self, client: "Client", group: str) -> None:
        self._client = client
        self._group = group

    def __getattr__(self, attribute: str) -> "Action":
        if attribute.startswith("_"):
            raise AttributeError(attribute)
        name = f"{self._group}.{attribute.replace('_', '-')}"
        info = self._client._action_info(name)
        if info is None:
            return Action(self._client, name)
        # A class of its own per action, so help() and inspect show the action's description.
        action_class = type(attribute, (Action,), {"__doc__": info.description, "__module__": __name__})
        return action_class(self._client, name)

    def __dir__(self) -> list[str]:
        """The actions in this group, with dashes shown as underscores. Asks rsimgv for them."""
        prefix = f"{self._group}."
        names = [name[len(prefix):].replace("-", "_") for name in self._client.list_actions() if name.startswith(prefix)]
        return sorted(names)

    def __repr__(self) -> str:
        return f"<action group {self._group}>"


class Action:
    """An action rsimgv runs on the active window when it is called."""

    def __init__(self, client: "Client", name: str) -> None:
        self._client = client
        self.name = name

    def __call__(self, value: float | None = None) -> None:
        """Runs the action, passing ``value`` if it takes one: ``client.action.video.seek(-30)``."""
        self._client.run_action(self.name, value)

    def __repr__(self) -> str:
        return f"<action {self.name}>"


class Client:
    """A client for one running rsimgv.

    ``Client()`` talks to the Unix socket in the runtime directory, which is what rsimgv serves by default. Pass a
    transport to use another one: ``Client(TcpTransport(7880))`` or ``Client(HttpTransport(7881))``. The ``from_unix``,
    ``from_tcp`` and ``from_http`` constructors do the same.
    """

    def __init__(self, transport: Transport | None = None) -> None:
        self._transport = transport if transport is not None else UnixTransport()
        self._ids = itertools.count(1)
        self.action = ActionNamespace(self)
        self.tabs = TabsNamespace(self)
        self.bookmarks = BookmarksNamespace(self)
        self._action_infos: dict[str, ActionInfo] | None = None
        self.navigate = NavigateNamespace(self)
        self.files = FilesNamespace(self)
        self.metadata = MetadataNamespace(self)
        self.state = StateNamespace(self)
        self.settings = SettingsNamespace(self)

    @property
    def current_path(self) -> str | None:
        """The folder the window shows, or None before any is shown."""
        return self.call("window.info")["current_path"]

    @property
    def selected_file(self) -> str | None:
        """The name of the item selected in the selected tab, or None if nothing is selected. Setting it selects
        that item in the selected tab."""
        return self.call("window.info")["selected_file"]

    @selected_file.setter
    def selected_file(self, file: str | os.PathLike[str]) -> None:
        self.files.select(file)

    @property
    def show_sidebar(self) -> bool:
        """Whether the sidebar is shown. Setting it shows or hides the sidebar."""
        return bool(self.call("window.info")["show_sidebar"])

    @show_sidebar.setter
    def show_sidebar(self, shown: bool) -> None:
        self.call("window.set_show_sidebar", show=bool(shown))

    @classmethod
    def from_unix(cls, path: str | Path | None = None, timeout: float = 10.0) -> "Client":
        """A client on rsimgv's Unix socket. With no path, the socket in the runtime directory."""
        return cls(UnixTransport(path, timeout))

    @classmethod
    def from_tcp(cls, port: int, host: str = DEFAULT_TCP_HOST, timeout: float = 10.0) -> "Client":
        """A client on rsimgv's localhost TCP port."""
        return cls(TcpTransport(port, host, timeout))

    @classmethod
    def from_http(cls, port: int, host: str = DEFAULT_TCP_HOST, timeout: float = 10.0) -> "Client":
        """A client on rsimgv's localhost HTTP port."""
        return cls(HttpTransport(port, host, timeout))

    def call(self, method: str, **params: Any) -> dict:
        """Sends one request and returns its result. Raises ``IpcError`` (or a subclass) for an error answer."""
        response = self._transport.exchange({"id": next(self._ids), "method": method, "params": params})
        error = response.get("error")
        if error is not None:
            exception_class = _ERRORS.get(error.get("code", ""), IpcError)
            exception = exception_class(error.get("code", ""), error.get("message", ""))
            if isinstance(exception, UnknownAction):
                exception.name = str(params.get("name", ""))
            raise exception
        return response.get("result", {})

    def describe_actions(self) -> list[ActionInfo]:
        """Every action rsimgv has, with its label, description, scope, icon and default keys."""
        return [
            ActionInfo(
                name=action["name"],
                label=action["label"],
                description=action["description"],
                scope=action["scope"],
                icon=action["icon"],
                defaults=tuple(action["defaults"]),
            )
            for action in self.call("actions.list")["actions"]
        ]

    def list_actions(self) -> list[str]:
        """The name of every action rsimgv has."""
        return [action.name for action in self.describe_actions()]

    def _action_info(self, name: str) -> ActionInfo | None:
        """The catalogue entry for an action. The catalogue is fetched once; if rsimgv cannot be reached, nothing is
        known yet and the next lookup asks again."""
        if self._action_infos is None:
            try:
                self._action_infos = {info.name: info for info in self.describe_actions()}
            except (RsimgvError, ConnectionFailed):
                return None
        return self._action_infos.get(name)

    def run_action(self, name: str, value: float | None = None) -> None:
        """Runs the action ``name``, passing ``value`` to an action that takes one, such as ``video.seek``."""
        if value is None:
            self.call("actions.run", name=name)
        else:
            self.call("actions.run", name=name, value=float(value))

    def screenshot(self) -> Screenshot:
        """Captures the picture, or the video's current frame, that the viewer shows, at its own size. Raises
        ``NoMedia`` when the viewer is closed or a video has no frame yet."""
        result = self.call("view.screenshot")
        return Screenshot(
            format=result["format"],
            width=int(result["width"]),
            height=int(result["height"]),
            data=base64.b64decode(result["data"]),
        )

    def get_metadata(self, *paths: str | os.PathLike[str]) -> list[FileMetadata]:
        """The metadata of each path, as :meth:`MetadataNamespace.get` gives it."""
        return self.metadata.get(*paths)

    def get_file_state(self, path: str | os.PathLike[str]) -> FileState:
        """The state saved for the file at ``path``."""
        return self.state.get(path)

    def set_file_state(
        self,
        path: str | os.PathLike[str],
        values: dict[str, Any] | None = None,
        **changes: Any,
    ) -> FileState:
        """Sets the state of the file at ``path``, as :meth:`StateNamespace.update` does."""
        return self.state.update(path, values, **changes)

    def reset_file_state(self, path: str | os.PathLike[str], *keys: str) -> FileState:
        """Removes keys from the state of the file at ``path``, as :meth:`StateNamespace.remove` does."""
        return self.state.remove(path, *keys)

    def list_settings(self) -> dict[str, Any]:
        """Every application setting, as :meth:`SettingsNamespace.list` gives them."""
        return self.settings.list()

    def describe_settings(self) -> list[SettingInfo]:
        """Every application setting, as :meth:`SettingsNamespace.describe` gives them."""
        return self.settings.describe()

    def get_setting(self, key: str, default: Any = _MISSING) -> Any:
        """The current value of the application setting ``key``."""
        return self.settings.get(key, default)

    def set_setting(self, key: str, value: Any) -> Any:
        """Sets the application setting ``key``, or resets it for None."""
        return self.settings.set(key, value)

    def update_settings(self, values: dict[str, Any] | None = None, **changes: Any) -> dict[str, Any]:
        """Sets several application settings at once."""
        return self.settings.update(values, **changes)

    def reset_settings(self, *keys: str) -> None:
        """Resets these application settings, or every setting with no keys."""
        self.settings.reset(*keys)


__all__ = [
    "Action",
    "ActionGroup",
    "ActionInfo",
    "ActionNamespace",
    "ActionNotRun",
    "BookmarksNamespace",
    "Client",
    "ConnectionFailed",
    "FileInfo",
    "FileMetadata",
    "FileState",
    "FilesNamespace",
    "HttpTransport",
    "IpcError",
    "MediaInfo",
    "MediaMetadata",
    "MetadataNamespace",
    "NavigateNamespace",
    "NoMedia",
    "NoSuchFile",
    "NoSuchSetting",
    "NoSuchTab",
    "NotAFolder",
    "RequestRefused",
    "RsimgvError",
    "RsimgvModel",
    "Screenshot",
    "SettingInfo",
    "SettingsNamespace",
    "StateNamespace",
    "TabsNamespace",
    "TcpTransport",
    "Transport",
    "UnixTransport",
    "UnknownAction",
    "default_socket_path",
]
