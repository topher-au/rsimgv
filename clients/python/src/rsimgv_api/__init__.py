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
"""

from .client import (
    ActionInfo,
    ActionNotRun,
    BookmarksNamespace,
    Client,
    FilesNamespace,
    NavigateNamespace,
    FileInfo,
    IpcError,
    NoSuchFile,
    NoSuchTab,
    NotAFolder,
    RsimgvError,
    TabsNamespace,
    UnknownAction,
)
from .transport import (
    ConnectionFailed,
    HttpTransport,
    RequestRefused,
    TcpTransport,
    Transport,
    UnixTransport,
    default_socket_path,
)

__all__ = [
    "ActionInfo",
    "ActionNotRun",
    "Client",
    "ConnectionFailed",
    "FileInfo",
    "FilesNamespace",
    "NavigateNamespace",
    "IpcError",
    "NoSuchFile",
    "NoSuchTab",
    "NotAFolder",
    "RequestRefused",
    "RsimgvError",
    "BookmarksNamespace",
    "TabsNamespace",
    "UnknownAction",
    "HttpTransport",
    "TcpTransport",
    "Transport",
    "UnixTransport",
    "default_socket_path",
]
