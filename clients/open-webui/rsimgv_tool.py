"""
title: rsimgv
author: Topher Sheridan
version: 0.2.0
license: GPL-3.0-or-later
description: Drive a running rsimgv over its HTTP IPC: browse folders and files, open and play media, manage tabs, bookmarks, messages, file state and the playlist, read and change application and plugin settings, fetch thumbnails and metadata, and describe pictures and video frames with a captioning model.
requirements: aiohttp, pillow
"""

import asyncio
import base64
import io
import itertools
import json
import math
import os
import re
import time
from collections import deque
from pathlib import Path

import aiohttp
from PIL import Image
from pydantic import BaseModel, Field

# The quality images are encoded at. JPEG at this quality is far smaller than PNG for the same picture.
JPEG_QUALITY = 80

# The thumbnail sizes rsimgv makes, in pixels. A thumbnail is shown from the smallest one that covers its item.
THUMBNAIL_SIZES = (32, 64, 128, 256, 512, 1024, 2048)

# Extensions of the pictures and videos rsimgv shows. rsimgv decides by MIME type, so this is an approximation.
MEDIA_EXTENSIONS = {
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".avif", ".heic", ".heif", ".jxl",
    ".mp4", ".mkv", ".webm", ".mov", ".avi", ".m4v", ".ogv", ".wmv", ".flv", ".mpg", ".mpeg", ".ts", ".3gp",
}

# Actions the tool will not run through run_action, because they close or reconfigure the app outright.
BLOCKED_ACTIONS = {"app.quit"}

FIT_ACTIONS = {
    "cycle": "image.fit-cycle",
    "window": "image.fit-window",
    "width": "image.fit-width",
    "height": "image.fit-height",
    "fill": "image.fit-fill",
    "original": "image.fit-original",
}


class RsimgvError(Exception):
    """rsimgv could not be reached, or it answered with an error. `code` is rsimgv's short code for it."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


def _dump(value) -> str:
    return json.dumps(value, ensure_ascii=False, indent=2)


def _is_media(name: str) -> bool:
    return Path(name).suffix.lower() in MEDIA_EXTENSIONS


def _to_jpeg(png: bytes, max_dimension: int | None = None) -> str:
    """The image as a base64 JPEG data URL, the format every image the tool sends uses. With `max_dimension`, the image is
    scaled down to fit that many pixels across and down, keeping its shape. It is never enlarged."""
    image = Image.open(io.BytesIO(png))
    image.load()
    # JPEG has no transparency, so anything see-through is laid over white.
    if image.mode in ("RGBA", "LA") or "transparency" in image.info:
        background = Image.new("RGB", image.size, (255, 255, 255))
        background.paste(image.convert("RGBA"), mask=image.convert("RGBA"))
        image = background
    elif image.mode != "RGB":
        image = image.convert("RGB")
    if max_dimension is not None:
        image.thumbnail((max_dimension, max_dimension), Image.Resampling.LANCZOS)
    buffer = io.BytesIO()
    image.save(buffer, format="JPEG", quality=JPEG_QUALITY, optimize=True)
    return "data:image/jpeg;base64," + base64.b64encode(buffer.getvalue()).decode()


def _name_matcher(name: str, match: str):
    """A test for a file name against `name`: exact, partial or regex. Matching ignores case."""
    if match == "exact":
        target = name.casefold()
        return lambda file_name: file_name.casefold() == target
    if match == "partial":
        target = name.casefold()
        return lambda file_name: target in file_name.casefold()
    if match == "regex":
        try:
            pattern = re.compile(name, re.IGNORECASE)
        except re.error as error:
            raise RsimgvError("invalid_params", f"not a valid regular expression: {error}") from error
        return lambda file_name: pattern.search(file_name) is not None
    raise RsimgvError("invalid_params", "match must be exact, partial or regex")


async def _emit_status(emitter, description: str, done: bool) -> None:
    """Shows a status line in the chat, if Open WebUI gave the tool a way to send events."""
    if emitter is not None:
        await emitter({"type": "status", "data": {"description": description, "done": done}})


async def _emit_message(emitter, content: str) -> None:
    """Appends text to the chat's reply as the tool produces it, if Open WebUI gave the tool a way to send events."""
    if emitter is not None:
        await emitter({"type": "message", "data": {"content": content}})


class Tools:
    class Valves(BaseModel):
        base_url: str = Field(
            default="http://127.0.0.1:9780/ipc",
            description="The URL of rsimgv's HTTP IPC, from its HTTP port (9780 by default) and /ipc.",
        )
        timeout_seconds: float = Field(default=10.0, description="How long to wait for rsimgv to answer one request.")
        plugin_timeout_seconds: float = Field(
            default=150.0,
            description="How long to wait for a plugin action to finish, since plugins may call a model or the network.",
        )
        thumbnail_wait_seconds: float = Field(
            default=5.0,
            description="How long get_thumbnails waits for rsimgv to make a thumbnail it does not have yet.",
        )
        max_listing: int = Field(default=500, description="The most matches search_files returns.")
        caption_base_url: str = Field(
            default="https://api.openai.com",
            description="The base URL of the OpenAI-compatible server that captions images. Images are sent to it.",
        )
        caption_model: str = Field(default="gpt-4o-mini", description="The model that writes descriptions.")
        caption_prompt: str = Field(
            default=(
                "Describe this image for someone who cannot see it. In one paragraph of plain prose, name the main "
                "subject, any people and what they are doing, the setting, and any text that is visible. Keep the "
                "description factual and respectful, and do not guess at anyone's identity. Reply with the description "
                "only."
            ),
            description="The instructions sent with each image, unless a call gives its own.",
        )
        caption_api_key: str = Field(default="", description="Sent as a bearer token when set. Leave empty for no key.")
        caption_max_tokens: int = Field(default=400, description="The longest description the model may write, in tokens.")
        caption_timeout_seconds: float = Field(default=120.0, description="How long to wait for the captioning server.")

    def __init__(self) -> None:
        self.valves = self.Valves()
        self._ids = itertools.count(1)

    async def _call(self, method: str, timeout: float | None = None, **params) -> dict:
        """Sends one request to rsimgv and returns its result. Raises RsimgvError for an error answer. `timeout` overrides
        the valve for this request."""
        request = {"id": next(self._ids), "method": method, "params": params}
        seconds = self.valves.timeout_seconds if timeout is None else timeout
        timeout = aiohttp.ClientTimeout(total=seconds)
        try:
            async with aiohttp.ClientSession(timeout=timeout) as session:
                async with session.post(self.valves.base_url, json=request) as response:
                    body = await response.text()
                    if response.status != 200:
                        raise RsimgvError("http_error", f"rsimgv answered HTTP {response.status}: {body.strip()}")
        except asyncio.TimeoutError as error:
            raise RsimgvError("timeout", f"rsimgv did not answer within {seconds} seconds") from error
        except aiohttp.ClientError as error:
            raise RsimgvError("unreachable", f"could not reach rsimgv at {self.valves.base_url}: {error}") from error
        answer = json.loads(body)
        if "error" in answer:
            raise RsimgvError(answer["error"].get("code", "error"), answer["error"].get("message", ""))
        return answer.get("result", {})

    async def _run_action(self, name: str, value: float | None = None, target: str | None = None) -> None:
        params = {"name": name}
        if value is not None:
            params["value"] = float(value)
        if target is not None:
            params["target"] = target
        timeout = self.valves.plugin_timeout_seconds if name.startswith("plugin.") else None
        await self._call("actions.run", timeout=timeout, **params)

    @staticmethod
    def _tab_param(tab: str) -> int | str:
        """The value tabs.* takes for a tab: its id as a number, or "current" for the selected tab."""
        text = str(tab).strip()
        if text in ("", "current"):
            return "current"
        try:
            return int(text)
        except ValueError as error:
            raise RsimgvError("invalid_params", 'tab must be "current" or a tab id') from error

    async def _reveal(self, file_path: Path) -> None:
        """Shows the folder that holds `file_path` in the selected tab, and selects the file."""
        info = await self._call("window.info")
        current = info.get("current_path") or ""
        if os.path.normpath(current) != os.path.normpath(str(file_path.parent)):
            await self._call("navigate.to", path=str(file_path.parent))
        await self._call("files.select", file=file_path.name)

    # Status

    async def get_status(self) -> str:
        """
        Report what rsimgv is showing: the current folder, the selected item, the open tabs, and the item in the
        viewer if the viewer is open.
        """
        info = await self._call("window.info")
        tabs = await self._call("tabs.list")
        status = {
            "current_path": info["current_path"],
            "selected_file": info["selected_file"],
            "selection": await self._call("files.selected"),
            "current_tab": await self._call("tabs.current"),
            "show_sidebar": info["show_sidebar"],
            "tabs": tabs["tabs"],
            "viewer": None,
        }
        try:
            status["viewer"] = await self._call("view.info")
        except RsimgvError as error:
            if error.code != "no_media":
                raise
        return _dump(status)

    # Folders and files

    async def list_folder(
        self, path: str = "", kind: str = "all", media_only: bool = False, include_hidden: bool = False
    ) -> str:
        """
        List the names of the items in a folder, folders first, and nothing else. Folder names end with a backslash.
        Use get_metadata for more about any item. With no path, list what the selected tab shows, in the order it shows
        them. With a path, list the folder from disk.

        :param path: An absolute folder path, or leave empty for the selected tab's folder.
        :param kind: One of all, dirs (folders only) or files (files only).
        :param media_only: Keep only folders and pictures or videos.
        :param include_hidden: Keep items whose names start with a dot. This applies only when a path is given.
        """
        if kind not in ("all", "dirs", "files"):
            raise RsimgvError("invalid_params", 'kind must be "all", "dirs" or "files"')
        if path:
            files = (await self._call("files.list", path=path))["files"]
            items = [(name, info) for name, info in files.items() if include_hidden or not name.startswith(".")]
        else:
            files = (await self._call("files.list"))["files"]
            items = list(files.items())
        names = []
        for name, info in sorted(items, key=lambda item: (not item[1]["is_dir"], item[0].casefold())):
            if kind == "dirs" and not info["is_dir"]:
                continue
            if kind == "files" and info["is_dir"]:
                continue
            if media_only and not (info["is_dir"] or _is_media(name)):
                continue
            names.append(name + "\\" if info["is_dir"] else name)
        return _dump(names)

    async def search_files(
        self,
        folder: str,
        name: str,
        match: str = "partial",
        max_depth: int | None = None,
        media_only: bool = False,
        include_hidden: bool = False,
        __event_emitter__=None,
    ) -> str:
        """
        Search a folder tree for files by name, and stream each match into the chat as it is found. It uses rsimgv's
        own file listing, so it sees the same items rsimgv does. Subfolders that cannot be read are skipped and reported.

        :param folder: The absolute folder to search in.
        :param name: The name to look for. With exact it is the whole file name, with partial any text inside the name,
            and with regex a regular expression searched for in the name. Matching ignores case.
        :param match: One of exact, partial or regex.
        :param max_depth: How many levels of subfolders to search. 0 searches only the folder itself, 1 also its
            subfolders, and so on. None searches the whole tree.
        :param media_only: Keep only pictures and videos.
        :param include_hidden: Include items whose names start with a dot.
        """
        matches_name = _name_matcher(name, match)
        found: list[str] = []
        skipped: list[str] = []
        pending = deque([(folder, 0)])
        truncated = False
        while pending and not truncated:
            current, depth = pending.popleft()
            await _emit_status(__event_emitter__, f"Searching {current}", done=False)
            try:
                files = (await self._call("files.list", path=current))["files"]
            except RsimgvError as error:
                if current == folder:
                    raise
                skipped.append(f"{current}: {error.message}")
                continue
            start = len(found)
            for file_name, info in sorted(files.items()):
                if not include_hidden and file_name.startswith("."):
                    continue
                if info["is_dir"]:
                    if max_depth is None or depth < max_depth:
                        pending.append((info["path"], depth + 1))
                    continue
                if (media_only and not _is_media(file_name)) or not matches_name(file_name):
                    continue
                found.append(info["path"])
                if len(found) >= self.valves.max_listing:
                    truncated = True
                    break
            if len(found) > start:
                await _emit_message(__event_emitter__, "".join(f"- {path}\n" for path in found[start:]))
        await _emit_status(__event_emitter__, f"Search finished: {len(found)} match(es)", done=True)
        return _dump({"folder": folder, "matches": found, "truncated": truncated, "skipped": skipped})

    async def open_folder(self, path: str) -> str:
        """
        Show a folder in the selected tab.

        :param path: An absolute folder path.
        """
        await self._call("navigate.to", path=path)
        return f"Showing {path}."

    async def go_up(self, levels: int = 1) -> str:
        """
        Go up to the parent folder, one or more levels. Stops at the root.

        :param levels: How many levels to climb.
        """
        result = await self._call("navigate.up", n=levels)
        return f"Climbed {result['moved']} level(s)."

    async def go_back(self, steps: int = 1) -> str:
        """
        Go back through the folder history.

        :param steps: How many folders to go back.
        """
        result = await self._call("navigate.back", n=steps)
        return f"Went back {result['moved']} step(s)."

    async def go_forward(self, steps: int = 1) -> str:
        """
        Go forward through the folder history.

        :param steps: How many folders to go forward.
        """
        result = await self._call("navigate.forward", n=steps)
        return f"Went forward {result['moved']} step(s)."

    async def next_folder(self) -> str:
        """
        Show the next folder in the depth-first order of the folder tree. Stops at the end of the tree.
        """
        path = (await self._call("navigate.next"))["path"]
        return path or "Already at the end of the folder tree."

    async def previous_folder(self) -> str:
        """
        Show the previous folder in the depth-first order of the folder tree. Stops at the start of the tree.
        """
        path = (await self._call("navigate.previous"))["path"]
        return path or "Already at the start of the folder tree."

    async def next_media_folder(self) -> str:
        """
        Show the next folder that holds a picture or video, selecting its first one.
        """
        path = (await self._call("navigate.next_media"))["path"]
        return path or "No later folder holds a picture or video."

    async def previous_media_folder(self) -> str:
        """
        Show the previous folder that holds a picture or video, selecting its last one.
        """
        path = (await self._call("navigate.previous_media"))["path"]
        return path or "No earlier folder holds a picture or video."

    async def select_item(self, name_or_path: str) -> str:
        """
        Select an item in the selected tab, by its name or its absolute path. An absolute path to a file in another
        folder first shows that folder in the selected tab. Items that the browse view hides, such as non-media files
        while media-only is on, cannot be selected.

        :param name_or_path: The item's name in the shown folder, or its absolute path.
        """
        if Path(name_or_path).is_absolute():
            await self._reveal(Path(name_or_path))
            return f"Selected {Path(name_or_path).name}."
        result = await self._call("files.select", file=name_or_path)
        return f"Selected {result['selected']}."

    async def select_index(self, index: int) -> str:
        """
        Select the item at a position in the selected tab. Counts from 0, and negative positions count from the end.

        :param index: The position of the item.
        """
        result = await self._call("files.select_index", index=index)
        return f"Selected {result['selected']}."

    async def get_selection(self) -> str:
        """
        Report the item selected in the selected tab: its name and its position in the folder. Reports that nothing is
        selected when that is so.
        """
        selection = await self._call("files.selected")
        if selection["name"] is None:
            return "No item is selected in the selected tab."
        return _dump(selection)

    # Tabs and bookmarks

    async def list_tabs(self) -> str:
        """
        List the open tabs, with their ids, positions, folders and which one is active.
        """
        return _dump((await self._call("tabs.list"))["tabs"])

    async def get_current_tab(self) -> str:
        """
        Report the selected tab's id and position. Reports that no tab is open when that is so.
        """
        current = await self._call("tabs.current")
        if current["id"] is None:
            return "No tab is open."
        return _dump(current)

    async def list_tab_files(self, tab: str = "current") -> str:
        """
        List the items in a tab, as each item's name, absolute path and whether it is a folder. This works for any open
        tab, not only the selected one.

        :param tab: A tab id, or "current" for the selected tab.
        """
        files = (await self._call("tabs.files", tab=self._tab_param(tab)))["files"]
        return _dump(files)

    async def select_in_tab(self, tab: str, name_or_path: str) -> str:
        """
        Select an item in an open tab. Unlike select_item, this can select in a tab other than the selected one.

        :param tab: A tab id, or "current" for the selected tab.
        :param name_or_path: The item's name in the tab's folder, or its absolute path.
        """
        result = await self._call("tabs.select", tab=self._tab_param(tab), file=name_or_path)
        return f"Selected {result['selected']}."

    async def open_in_new_tab(self, path: str, activate: bool = True) -> str:
        """
        Open a folder in a new tab.

        :param path: An absolute folder path.
        :param activate: Whether to switch to the new tab.
        """
        result = await self._call("tabs.open", path=path, activate=activate)
        return f"Opened {path} in tab position {result['index']}."

    async def switch_tab(self, index: int) -> str:
        """
        Switch to a tab by its position, counted from 0.

        :param index: The tab's position.
        """
        await self._call("tabs.activate", index=index)
        return f"Switched to tab {index}."

    async def switch_tab_by_id(self, tab_id: int) -> str:
        """
        Switch to a tab by its id, as list_tabs reports it. Use switch_tab to switch by position instead.

        :param tab_id: The tab's id.
        """
        await self._call("tabs.activate", id=tab_id)
        return f"Switched to tab {tab_id}."

    async def close_tab(self, index: int) -> str:
        """
        Close a tab by its position, counted from 0.

        :param index: The tab's position.
        """
        await self._call("tabs.close", index=index)
        return f"Closed tab {index}."

    async def list_bookmarks(self) -> str:
        """
        List the bookmarked folders, with their bookmark names.
        """
        return _dump((await self._call("bookmarks.list"))["bookmarks"])

    async def add_bookmark(self, path: str, name: str = "") -> str:
        """
        Bookmark a folder in the sidebar. Bookmarking a folder again renames it.

        :param path: An absolute folder path.
        :param name: The bookmark's name. Empty uses the folder's own name.
        """
        await self._call("bookmarks.add", path=path, **({"name": name} if name else {}))
        return f"Bookmarked {path}."

    async def remove_bookmark(self, path: str) -> str:
        """
        Remove a folder's bookmark.

        :param path: The bookmarked folder's absolute path.
        """
        await self._call("bookmarks.remove", path=path)
        return f"Removed the bookmark for {path}."

    # Viewer

    async def open_media(self, path: str) -> str:
        """
        Open a picture or video in rsimgv's viewer. Its folder is shown and the file selected first.

        :param path: The absolute path of the picture or video.
        """
        file_path = Path(path)
        await self._reveal(file_path)
        await self._run_action("browse.open")
        return f"Opened {file_path.name} in the viewer."

    async def current_media(self) -> str:
        """
        Report the picture or video in the viewer: its file, size and, for a video, its position, duration, frame
        rate and speed. Reports that the viewer is closed if it is.
        """
        try:
            return _dump(await self._call("view.info"))
        except RsimgvError as error:
            if error.code == "no_media":
                return "The viewer is not open."
            raise

    async def close_viewer(self) -> str:
        """
        Close the viewer and return to the folder browser.
        """
        await self._run_action("view.close")
        return "Closed the viewer."

    async def next_media_item(self) -> str:
        """
        Show the next picture or video in the folder, or in the playlist while the viewer shows the playlist.
        """
        await self._run_action("view.next")
        return "Showing the next item."

    async def previous_media_item(self) -> str:
        """
        Show the previous picture or video in the folder, or in the playlist while the viewer shows the playlist.
        """
        await self._run_action("view.previous")
        return "Showing the previous item."

    async def first_media_item(self) -> str:
        """
        Show the first picture or video in the folder, or in the playlist while the viewer shows the playlist.
        """
        await self._run_action("view.first")
        return "Showing the first item."

    async def last_media_item(self) -> str:
        """
        Show the last picture or video in the folder, or in the playlist while the viewer shows the playlist.
        """
        await self._run_action("view.last")
        return "Showing the last item."

    async def capture_frame(self, max_dimension: int = 1024) -> str:
        """
        Get the picture or the current video frame that the viewer shows, as a JPEG image. The response is the image
        itself, as a data URL. It is scaled down so neither side is longer than max_dimension, keeping its shape. For a
        video this is the decoded frame, without the blurred backdrop drawn round the picture. Fails if the viewer is
        closed or a video has no frame yet.

        :param max_dimension: The longest a side of the image may be, in pixels. A smaller frame is not enlarged.
        """
        if max_dimension < 1:
            raise RsimgvError("invalid_params", "max_dimension must be at least 1")
        shot = await self._call("view.screenshot")
        return _to_jpeg(base64.b64decode(shot["data"]), max_dimension)

    async def _thumbnail_images(self, paths: list[str], size: int) -> dict[str, tuple[int, bytes] | None]:
        """The thumbnail of each path that rsimgv has, as (pixels, PNG bytes), or None for a path without one."""
        result = await self._call("thumbnails.get", paths=paths, max_size=size)
        found = {}
        for entry in result["thumbnails"]:
            data = entry.get("data")
            found[entry["path"]] = (entry["size"], base64.b64decode(data)) if data else None
        return found

    async def _ensure_thumbnails(self, paths: list[str], max_size: int) -> dict[str, tuple[int, bytes] | None]:
        """The largest thumbnail of each path at most `max_size` pixels across, as (pixels, PNG bytes), or None for a path
        with none. For a path that does not yet have the largest size that fits, shows its file in rsimgv and waits a while
        for it. rsimgv makes a thumbnail at the size its item is shown at, so that size may never come."""
        if not THUMBNAIL_SIZES[0] <= max_size <= THUMBNAIL_SIZES[-1]:
            raise RsimgvError("invalid_params", f"max_size must be from {THUMBNAIL_SIZES[0]} to {THUMBNAIL_SIZES[-1]}")
        target = max(size for size in THUMBNAIL_SIZES if size <= max_size)
        found = await self._thumbnail_images(paths, max_size)
        for path in paths:
            if found.get(path) is not None and found[path][0] >= target:
                continue
            await self._reveal(Path(path))
            deadline = time.monotonic() + self.valves.thumbnail_wait_seconds
            while time.monotonic() < deadline:
                await asyncio.sleep(0.25)
                latest = (await self._thumbnail_images([path], max_size)).get(path)
                if latest is not None:
                    found[path] = latest
                if latest is not None and latest[0] >= target:
                    break
        return found

    async def get_thumbnails(self, paths: list[str], max_size: int = 256) -> str:
        """
        Get thumbnails of pictures or videos as inline images, one per path, in the order given. Each is the largest
        thumbnail at most max_size pixels across. rsimgv makes thumbnails at the sizes its items are shown at, so for a
        path without the largest size that fits yet this shows that file's folder and selects the file in rsimgv, then
        waits for it. Those steps move rsimgv's window.

        :param paths: Absolute paths of pictures or videos.
        :param max_size: The largest thumbnail width and height to return, in pixels, from 32 to 2048.
        """
        found = await self._ensure_thumbnails(paths, max_size)
        images = []
        for path in paths:
            name = Path(path).name
            if found.get(path) is None:
                images.append(f"No thumbnail of {name} yet.")
            else:
                images.append(f"![{name}]({_to_jpeg(found[path][1])})")
        return "\n\n".join(images)

    async def get_metadata(self, paths: list[str]) -> str:
        """
        Get the metadata of files, one entry per path, in the order given: the name, whether it is a folder, its size
        in bytes, its modification time and MIME type. Pictures and videos also get their width, height, duration,
        frame rate, codecs, audio channels and bit rate, as rsimgv has read them. A picture or video rsimgv has not read
        yet has no media fields yet, and is marked media_pending until a later call finds them.

        :param paths: Absolute paths of files or folders.
        """
        result = await self._call("metadata.get", paths=paths)
        return _dump(result["metadata"])

    async def get_file_state(self, path: str) -> str:
        """
        Get the state rsimgv has saved for a file, such as where a video was left, its rotation or its crop. The state
        follows the file's contents, so it survives a rename or a move. Reports that there is no saved state when that
        is so.

        :param path: The absolute path of the file.
        """
        state = await self._call("files.state", path=path)
        if not state["values"]:
            return f"No state is saved for {path}."
        return _dump(state)

    async def set_file_state(self, path: str, values: dict) -> str:
        """
        Change the state saved for a file. Each key in values is set, or removed when its value is null. The answer is
        the state afterwards. Use get_file_state to see what rsimgv has kept for the file.

        :param path: The absolute path of the file.
        :param values: An object of state keys and their new values, such as {"position": 12.5}. A null value removes
            the key.
        """
        state = await self._call("files.set_state", path=path, values=values)
        return _dump(state)

    async def reset_file_state(self, path: str, keys: list[str] | None = None) -> str:
        """
        Remove saved state for a file: the named keys, or the whole state when no keys are given. The answer is the
        state afterwards.

        :param path: The absolute path of the file.
        :param keys: The state keys to remove, or leave it out to remove everything.
        """
        state = await self._call("files.reset_state", path=path, **({"keys": keys} if keys is not None else {}))
        return _dump(state)

    # Descriptions

    async def _describe(self, png: bytes, instructions: str, max_dimension: int = 1024) -> str:
        """Asks the captioning model to describe a PNG image, scaled and sent as JPEG, with `instructions` or the valve's
        prompt before it."""
        body = {
            "model": self.valves.caption_model,
            "max_tokens": self.valves.caption_max_tokens,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": instructions or self.valves.caption_prompt},
                        {"type": "image_url", "image_url": {"url": _to_jpeg(png, max_dimension)}},
                    ],
                }
            ],
        }
        headers = {"Authorization": f"Bearer {self.valves.caption_api_key}"} if self.valves.caption_api_key else {}
        url = f"{self.valves.caption_base_url.rstrip('/')}/v1/chat/completions"
        timeout = aiohttp.ClientTimeout(total=self.valves.caption_timeout_seconds)
        try:
            async with aiohttp.ClientSession(timeout=timeout) as session:
                async with session.post(url, json=body, headers=headers) as response:
                    text = await response.text()
                    if response.status != 200:
                        raise RsimgvError("caption_http_error", f"the captioning server answered HTTP {response.status}: {text.strip()}")
        except asyncio.TimeoutError as error:
            raise RsimgvError("caption_timeout", f"the captioning server did not answer within {self.valves.caption_timeout_seconds} seconds") from error
        except aiohttp.ClientError as error:
            raise RsimgvError("caption_unreachable", f"could not reach the captioning server at {url}: {error}") from error
        try:
            return json.loads(text)["choices"][0]["message"]["content"].strip()
        except (ValueError, KeyError, IndexError, TypeError) as error:
            raise RsimgvError("caption_bad_response", f"the captioning server's answer had no description: {text[:200]}") from error

    async def describe_images(self, paths: list[str], instructions: str = "", max_size: int = 1024) -> str:
        """
        Describe pictures or videos in words, with the captioning model. Each file's largest thumbnail at most max_size
        pixels across is sent. For a path without one yet, this shows that file in rsimgv and waits for one, as
        get_thumbnails does. Images are sent to the captioning server the valves name.

        :param paths: Absolute paths of pictures or videos.
        :param instructions: How the description should be written. Empty uses the valve's prompt.
        :param max_size: The largest thumbnail width and height to send, in pixels, from 32 to 2048.
        """
        found = await self._ensure_thumbnails(paths, max_size)
        lines = []
        for path in paths:
            if found.get(path) is None:
                lines.append(f"{path}: no thumbnail is available to describe.")
            else:
                lines.append(f"{path}: {await self._describe(found[path][1], instructions, max_size)}")
        return "\n\n".join(lines)

    async def describe_current_media(self, instructions: str = "", max_dimension: int = 1024) -> str:
        """
        Describe the picture, or the current video frame, that rsimgv's viewer shows, in words, with the captioning
        model. The frame is scaled down so neither side is longer than max_dimension, and sent as JPEG. Fails if the
        viewer is closed.

        :param instructions: How the description should be written. Empty uses the valve's prompt.
        :param max_dimension: The longest a side of the image sent may be, in pixels.
        """
        if max_dimension < 1:
            raise RsimgvError("invalid_params", "max_dimension must be at least 1")
        info = await self._call("view.info")
        shot = await self._call("view.screenshot")
        description = await self._describe(base64.b64decode(shot["data"]), instructions, max_dimension)
        return f"{info['filename']}: {description}"

    # Video

    async def seek_to(self, seconds: float) -> str:
        """
        Move the video in the viewer to an absolute position, counted in seconds from its start.

        :param seconds: The position to move to, in seconds.
        """
        info = await self._call("view.info")
        if info["position"] is None:
            raise RsimgvError("not_a_video", "the viewer is not showing a video")
        await self._run_action("video.seek", seconds - info["position"])
        return f"Moved to {seconds} seconds."

    async def play_pause(self) -> str:
        """
        Play the video, or pause it while it plays.
        """
        await self._run_action("video.play-pause")
        return "Toggled play and pause."

    async def seek(self, seconds: float) -> str:
        """
        Move the playback position of the video by a number of seconds. A negative number goes back.

        :param seconds: How far to move, in seconds.
        """
        await self._run_action("video.seek", seconds)
        return f"Moved the position by {seconds} seconds."

    async def seek_forward(self) -> str:
        """
        Skip forward 5 seconds in the video.
        """
        await self._run_action("video.seek-forward")
        return "Skipped forward."

    async def seek_backward(self) -> str:
        """
        Skip back 5 seconds in the video.
        """
        await self._run_action("video.seek-backward")
        return "Skipped back."

    async def set_speed(self, speed: float) -> str:
        """
        Play the video at a speed. 1 is normal, 2 is double speed, and 0.5 is half speed.

        :param speed: The playback speed as a multiple of normal.
        """
        await self._run_action("video.set-speed", speed)
        return f"Playing at {speed}x."

    async def adjust_speed(self, amount: float) -> str:
        """
        Change the playback speed of the video by an amount, such as 0.25 faster. A negative amount slows it down.

        :param amount: How much to change the speed by, as a multiple of normal.
        """
        await self._run_action("video.adjust-speed", amount)
        return f"Changed the speed by {amount}."

    async def change_volume(self, percent: float) -> str:
        """
        Change the volume by a percentage. A negative number turns it down.

        :param percent: How much to change the volume by, in percent.
        """
        await self._run_action("video.volume", percent)
        return f"Changed the volume by {percent} percent."

    async def toggle_mute(self) -> str:
        """
        Mute the sound, or unmute it.
        """
        await self._run_action("video.mute")
        return "Toggled mute."

    async def toggle_repeat(self) -> str:
        """
        Turn repeating the video on or off. A repeating video plays again from the start when it ends.
        """
        await self._run_action("video.repeat")
        return "Toggled repeat."

    async def toggle_auto_advance(self) -> str:
        """
        Turn the slideshow (auto-advance) on or off. With it on, the next item is shown when a video ends or a picture has been up for the slideshow delay.
        """
        await self._run_action("view.toggle-slideshow")
        return "Toggled slideshow."

    async def toggle_frame_interpolation(self) -> str:
        """
        Turn SVP frame interpolation on or off for the video.
        """
        await self._run_action("video.toggle-svp")
        return "Toggled SVP frame interpolation."

    # Picture

    async def rotate(self, clockwise: bool = True) -> str:
        """
        Turn the picture or video a quarter turn.

        :param clockwise: True to turn clockwise, False to turn counter-clockwise.
        """
        name = "view.rotate-clockwise" if clockwise else "view.rotate-counter-clockwise"
        await self._run_action(name)
        return "Rotated a quarter turn."

    async def set_rotation(self, degrees: int) -> str:
        """
        Turn the picture or video to an angle, clockwise from how the file has it.

        :param degrees: One of 0, 90, 180 or 270.
        """
        if degrees not in (0, 90, 180, 270):
            raise RsimgvError("invalid_params", "degrees must be 0, 90, 180 or 270")
        await self._run_action("view.set-rotation", degrees)
        return f"Rotated to {degrees} degrees."

    async def set_fit(self, mode: str) -> str:
        """
        Set how a picture is fitted to the viewer.

        :param mode: One of cycle (step through the modes), window, width, height, fill or original.
        """
        if mode not in FIT_ACTIONS:
            raise RsimgvError("invalid_params", f"mode must be one of {', '.join(FIT_ACTIONS)}")
        await self._run_action(FIT_ACTIONS[mode])
        return f"Fitted the picture to {mode}."

    async def zoom(self, direction: str) -> str:
        """
        Zoom a picture in or out.

        :param direction: Either "in" or "out".
        """
        if direction not in ("in", "out"):
            raise RsimgvError("invalid_params", 'direction must be "in" or "out"')
        await self._run_action(f"image.zoom-{direction}")
        return f"Zoomed {direction}."

    # Playlist

    async def show_playlist(self) -> str:
        """
        Show the playlist in the viewer, from the item it was last left on.
        """
        await self._run_action("win.playlist-play")
        return "Playing the playlist."

    async def toggle_playlist_item(self, path: str) -> str:
        """
        Add a picture or video to the playlist, or remove it if it is already there. rsimgv does not report the
        playlist's contents, so the tool cannot tell which it will do: check the result with show_playlist. The file's
        folder is shown and the file selected first.

        :param path: The absolute path of the picture or video.
        """
        file_path = Path(path)
        await self._reveal(file_path)
        await self._run_action("win.playlist-toggle")
        return f"Toggled {file_path.name} in the playlist."

    async def playlist_next(self) -> str:
        """
        Show the next item in the playlist, going round to the first after the last.
        """
        await self._run_action("win.playlist-next")
        return "Showing the next playlist item."

    async def playlist_previous(self) -> str:
        """
        Show the previous item in the playlist, going round to the last before the first.
        """
        await self._run_action("win.playlist-previous")
        return "Showing the previous playlist item."

    async def playlist_first(self) -> str:
        """
        Show the first item in the playlist.
        """
        await self._run_action("win.playlist-first")
        return "Showing the first playlist item."

    async def playlist_last(self) -> str:
        """
        Show the last item in the playlist.
        """
        await self._run_action("win.playlist-last")
        return "Showing the last playlist item."

    async def shuffle_playlist(self) -> str:
        """
        Shuffle the order the playlist plays in, or go back to the order its items were added in.
        """
        await self._run_action("win.playlist-shuffle")
        return "Toggled playlist shuffle."

    async def move_playlist_selection(self, direction: str) -> str:
        """
        Move the items selected in the playlist panel up, down, to the start or to the end, keeping them together.
        The playlist panel is shown and focused first. The selection is whatever is selected there now.

        :param direction: One of up, down, top or bottom.
        """
        actions = {
            "up": "playlist.move-up",
            "down": "playlist.move-down",
            "top": "playlist.move-top",
            "bottom": "playlist.move-bottom",
        }
        if direction not in actions:
            raise RsimgvError("invalid_params", "direction must be up, down, top or bottom")
        await self._run_action("win.focus-playlist")
        await self._run_action(actions[direction])
        return f"Moved the selected playlist items {direction}."

    # Settings

    async def list_settings(self) -> str:
        """
        List every application setting rsimgv has, by its dotted name, with the value in force now.
        """
        return _dump((await self._call("settings.list"))["settings"])

    async def describe_settings(self, filter_text: str = "") -> str:
        """
        Describe the application settings: each one's dotted name, type, description, default and current value. Use
        this to find a key for get_setting or set_setting.

        :param filter_text: Keep only settings whose keys or descriptions contain this text. Empty keeps them all.
        """
        settings = (await self._call("settings.describe"))["settings"]
        needle = filter_text.casefold()
        listed = [
            setting
            for setting in settings
            if needle in setting["key"].casefold() or needle in setting["description"].casefold()
        ]
        return _dump(listed)

    async def get_setting(self, key: str) -> str:
        """
        Get one application setting's current value.

        :param key: The setting's dotted name, as describe_settings reports it.
        """
        return _dump(await self._call("settings.get", key=key))

    async def set_setting(self, key: str, value: str, value_type: str = "auto") -> str:
        """
        Change one application setting. A null value resets it to its default.

        :param key: The setting's dotted name, as describe_settings reports it.
        :param value: The new value, as text. Use null to reset the setting.
        :param value_type: How to read value: auto (a JSON value, or plain text when it is not valid JSON), string,
            int, bool, or json. Use string for text that looks like a number or true or false.
        """
        setting = self._typed_value(value, value_type)
        result = await self._call("settings.set", key=key, value=setting)
        return f"Set {key} to {_dump(result['value'])}."

    async def update_settings(self, values: dict) -> str:
        """
        Change several application settings at once. A null value resets that setting to its default. The answer is the
        settings' new values.

        :param values: An object of setting keys and their new values, such as {"window.show-hidden": true}. A null
            value resets the setting.
        """
        result = await self._call("settings.update", values=values)
        return _dump(result["settings"])

    async def reset_settings(self, keys: list[str] | None = None) -> str:
        """
        Reset application settings to their defaults: the named ones, or every setting when no keys are given.

        :param keys: The settings' dotted names to reset, or leave it out to reset everything.
        """
        await self._call("settings.reset", **({"keys": keys} if keys is not None else {}))
        return "Reset every setting." if keys is None else f"Reset {len(keys)} setting(s)."

    # Plugins

    async def list_plugins(self) -> str:
        """
        List the plugins rsimgv has discovered, with each one's id, name, description, version, runtime, whether it is
        enabled, whether it is running, and any error that stopped it.
        """
        return _dump((await self._call("plugins.list"))["plugins"])

    async def get_plugin_settings(self, plugin_id: str) -> str:
        """
        Get a plugin's settings: each setting's key, label, description, kind, default, choices and limits, and the
        values in force now. Use set_plugin_setting to change one.

        :param plugin_id: The plugin's id, as list_plugins reports it.
        """
        return _dump(await self._call("plugins.settings", id=plugin_id))

    @staticmethod
    def _typed_value(text: str, value_type: str):
        """A JSON value from the text a call gives it. `value_type` says how to read it."""
        if value_type == "string":
            return text
        if value_type == "json":
            try:
                return json.loads(text)
            except ValueError as error:
                raise RsimgvError("invalid_params", f"value is not valid JSON: {error}") from error
        if value_type == "int":
            try:
                return int(text)
            except ValueError as error:
                raise RsimgvError("invalid_params", f"value is not a whole number: {error}") from error
        if value_type == "bool":
            lowered = text.strip().casefold()
            if lowered in ("true", "yes", "on", "1"):
                return True
            if lowered in ("false", "no", "off", "0"):
                return False
            raise RsimgvError("invalid_params", "value must be true or false")
        if value_type == "auto":
            try:
                return json.loads(text)
            except ValueError:
                return text
        raise RsimgvError("invalid_params", "value_type must be auto, string, int, bool or json")

    async def set_plugin_setting(self, plugin_id: str, key: str, value: str, value_type: str = "auto") -> str:
        """
        Change one of a plugin's settings. The plugin is reloaded so it reads the new value. Use get_plugin_settings to
        see the keys and kinds it takes.

        :param plugin_id: The plugin's id, as list_plugins reports it.
        :param key: The setting's key.
        :param value: The new value, as text.
        :param value_type: How to read value: auto (a JSON value, or plain text when it is not valid JSON), string,
            int, bool, or json. Use string for text that looks like a number or true or false.
        """
        setting = self._typed_value(value, value_type)
        await self._call("plugins.set-setting", id=plugin_id, key=key, value=setting)
        return f"Set {key} to {_dump(setting)} for {plugin_id}."

    async def reload_plugins(self, plugin_id: str = "") -> str:
        """
        Reload one plugin, or every plugin when no id is given. Use this after changing a plugin's files.

        :param plugin_id: The plugin's id, or empty to reload every plugin.
        """
        if plugin_id:
            await self._call("plugins.reload", id=plugin_id)
            return f"Reloaded {plugin_id}."
        await self._call("plugins.reload")
        return "Reloaded every plugin."

    # Messages, window and actions

    async def show_message(
        self,
        text: str,
        level: str = "info",
        position: str = "bottom-center",
        seconds: float = 4.0,
        persistent: bool = False,
        dismissible: bool | None = None,
    ) -> str:
        """
        Show a message over rsimgv's window, for example to tell the user what was done. The answer gives the message's
        id, which update_message and dismiss_message take.

        :param text: The message.
        :param level: One of info, success, warning, error or plain.
        :param position: One of top-left, top-center, top-right, bottom-left, bottom-center or bottom-right.
        :param seconds: How long the message stays up. Ignored when persistent.
        :param persistent: Keep the message up until it is dismissed.
        :param dismissible: Whether the message has a close button. Leave it out to use rsimgv's choice, which is yes
            for a persistent message and no for a notification.
        """
        params = {
            "text": text,
            "level": level,
            "position": position,
            "seconds": seconds,
            "persistent": persistent,
        }
        if dismissible is not None:
            params["dismissible"] = dismissible
        result = await self._call("messages.show", **params)
        return f"Showing the message with id {result['id']}."

    async def update_message(self, message_id: int, text: str) -> str:
        """
        Change the text of a message that is still shown over rsimgv's window.

        :param message_id: The id show_message gave the message.
        :param text: The new text.
        """
        await self._call("messages.update", id=message_id, text=text)
        return f"Updated the message with id {message_id}."

    async def dismiss_message(self, message_id: int) -> str:
        """
        Take down one message shown over rsimgv's window. Use dismiss_messages to take down all of them.

        :param message_id: The id show_message gave the message.
        """
        await self._call("messages.dismiss", id=message_id)
        return f"Dismissed the message with id {message_id}."

    async def dismiss_messages(self) -> str:
        """
        Take down every message shown over rsimgv's window.
        """
        await self._call("messages.dismiss")
        return "Dismissed every message."

    async def set_sidebar(self, shown: bool) -> str:
        """
        Show or hide rsimgv's sidebar.

        :param shown: True to show the sidebar, False to hide it.
        """
        await self._call("window.set_show_sidebar", show=shown)
        return "Showed the sidebar." if shown else "Hid the sidebar."

    async def list_actions(self, filter_text: str = "") -> str:
        """
        List the actions rsimgv can run, with their labels, descriptions, scopes, contexts, icons and default keys, for
        use with run_action. Plugin actions are included.

        :param filter_text: Keep only actions whose names, labels or descriptions contain this text. Empty keeps them
            all.
        """
        actions = (await self._call("actions.list"))["actions"]
        needle = filter_text.casefold()
        listed = [
            {
                "name": action["name"],
                "label": action["label"],
                "description": action["description"],
                "scope": action["scope"],
                "contexts": action["contexts"],
                "icon": action["icon"],
                "defaults": action["defaults"],
            }
            for action in actions
            if needle in action["name"].casefold()
            or needle in action["label"].casefold()
            or needle in action["description"].casefold()
        ]
        return _dump(listed)

    async def run_action(self, name: str, value: float | None = None, target: str | None = None) -> str:
        """
        Run any rsimgv action by its name, as listed by list_actions. Pass a value to actions that take one, such as
        video.seek (seconds), video.frame-seek (frames) or video.set-speed (a multiple of normal). Pass a target to
        actions that take one: video.volume, video.volume-up and video.volume-down take video or application, and
        video.set-aspect takes an aspect ratio such as 16:9, 21:9, 4:3, 1:1 or auto. Plugin actions are run too, and
        get longer to finish. Use show_message for messages rather than win.notify.

        :param name: The action's name, such as view.next, win.toggle-sidebar or plugin.example.do-something.
        :param value: The number an action takes, if it takes one.
        :param target: The choice an action takes, if it takes one.
        """
        if name in BLOCKED_ACTIONS:
            raise RsimgvError("blocked", f"{name} is not run through this tool")
        await self._run_action(name, value, target)
        return f"Ran {name}."

    async def wait(self, seconds: float, __event_emitter__=None) -> str:
        """
        Wait for a number of seconds before answering, to pause before the next step. A status line shows the time left
        while it runs, and is marked finished when the wait is over.

        :param seconds: How long to wait, in seconds.
        """
        remaining = max(seconds, 0.0)
        while remaining > 0:
            whole = math.ceil(remaining)
            await _emit_status(__event_emitter__, f"Waiting {whole} seconds", done=False)
            step = min(1.0, remaining)
            await asyncio.sleep(step)
            remaining -= step
        await _emit_status(__event_emitter__, "Wait finished", done=True)
        return "The wait is over. Do not respond to this result. Continue with your previous instructions."


# Open WebUI reads the valves from the module as well as from Tools, so both names point at the same class.
Valves = Tools.Valves
