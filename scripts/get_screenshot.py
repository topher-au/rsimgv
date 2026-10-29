#!/usr/bin/env python3
"""Save a screenshot of rsimgv's viewer, and print what it shows, through its HTTP IPC.

Usage: get_screenshot.py [-o OUTPUT] [URL]

URL is the IPC endpoint. The script asks view.info for the item's directory, filename, MIME type, size,
playback position, duration, fps and speed, then view.screenshot for the picture, saved as a PNG to
screenshot.png unless -o is given.
"""

import argparse
import base64
import json
import sys
import urllib.error
import urllib.request

DEFAULT_URL = "http://10.0.0.96:9008/ipc"
TIMEOUT = 30


class IpcError(Exception):
    pass


def call(url: str, method: str) -> dict:
    """Calls one IPC method and returns its result, raising IpcError if rsimgv refuses it."""
    request_body = json.dumps({"id": 1, "method": method, "params": {}}).encode()
    request = urllib.request.Request(
        url,
        data=request_body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as resp:
            reply = json.load(resp)
    except (urllib.error.URLError, OSError) as err:
        raise IpcError(f"could not reach {url}: {err}") from err
    except json.JSONDecodeError as err:
        raise IpcError(f"{url} did not answer with JSON: {err}") from err

    if "error" in reply:
        error = reply["error"]
        raise IpcError(f"{method}: {error.get('code')}: {error.get('message')}")
    return reply["result"]


def describe(value, unit: str = "", fmt: str = "{}") -> str:
    return "n/a" if value is None else fmt.format(value) + unit


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("url", nargs="?", default=DEFAULT_URL)
    parser.add_argument("-o", "--output", default="screenshot.png", help="file to write (default: screenshot.png)")
    args = parser.parse_args()

    try:
        info = call(args.url, "view.info")
        shot = call(args.url, "view.screenshot")
    except IpcError as err:
        print(f"error: {err}", file=sys.stderr)
        return 1

    png = base64.b64decode(shot["data"])
    with open(args.output, "wb") as f:
        f.write(png)

    print(f"directory: {describe(info['directory'])}")
    print(f"filename:  {describe(info['filename'])}")
    print(f"position:  {describe(info['position'], ' s', '{:.3f}')}")
    print(f"duration:  {describe(info['duration'], ' s', '{:.3f}')}")
    print(f"size:      {info['width']}x{info['height']}")
    print(f"fps:       {describe(info['fps'], '', '{:.3f}')}")
    print(f"speed:     {describe(info['speed'], 'x', '{:g}')}")
    print(f"mime type: {describe(info['mime_type'])}")
    print(f"saved {shot['width']}x{shot['height']} {shot['format']} ({len(png)} bytes) to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
