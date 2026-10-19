"""The client: actions, tabs and files, over whichever transport rsimgv is serving."""

from __future__ import annotations

import base64
import itertools
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Union

from .transport import ConnectionFailed, HttpTransport, TcpTransport, Transport, UnixTransport

DEFAULT_TCP_HOST = "127.0.0.1"

#: A tab id, or "current" for the selected tab.
TabRef = Union[int, str]


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


class NoMedia(IpcError):
    """The viewer is not showing a picture or a video frame to capture."""


_ERRORS: dict[str, type[IpcError]] = {
    "unknown_action": UnknownAction,
    "not_run": ActionNotRun,
    "no_such_tab": NoSuchTab,
    "no_such_file": NoSuchFile,
    "not_a_folder": NotAFolder,
    "no_media": NoMedia,
}


@dataclass(frozen=True)
class Screenshot:
    """The media the viewer showed, at its own size: ``data`` is the image file's bytes, in ``format``."""

    format: str
    width: int
    height: int
    data: bytes

    def save(self, path: str | os.PathLike[str]) -> Path:
        """Writes the image to ``path`` and returns it."""
        path = Path(path)
        path.write_bytes(self.data)
        return path


@dataclass(frozen=True)
class ActionInfo:
    """What rsimgv says about one action."""

    name: str
    label: str
    description: str
    scope: str
    icon: str
    defaults: tuple[str, ...]


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


@dataclass(frozen=True)
class FileInfo:
    """What rsimgv says about one item in a folder. ``size`` is in bytes, for a file; ``modified`` is in seconds
    since the Unix epoch. Either is None when rsimgv could not read it."""

    name: str
    path: str
    is_dir: bool
    size: int | None
    modified: float | None


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
