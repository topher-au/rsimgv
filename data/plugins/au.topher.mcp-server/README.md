# MCP Server

Serves the [Model Context Protocol](https://modelcontextprotocol.io) from inside rsimgv so an MCP client
can browse folders, drive the viewer, read what it shows and change settings.

The endpoint is `http://127.0.0.1:8765/mcp` (streamable HTTP, JSON responses). By default it listens on the
loopback interface only and rejects requests whose `Host` or `Origin` is not local. The tools can change
settings and run any action, so set a token if other local users share the machine.

## Connect Claude Code

```sh
claude mcp add --transport http rsimgv http://127.0.0.1:8765/mcp
```

With a token: add `--header "Authorization: Bearer <token>"`.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `port` | 8765 | Listening port; applies after the plugin reloads. |
| `bind_address` | 127.0.0.1 | Address to listen on. `0.0.0.0` accepts connections from the network. |
| `token` | empty | When set, clients must send it as a Bearer token. Required when `bind_address` is not loopback; the server refuses to start without one. |
| `read_only` | off | Hides every tool that is not marked read-only: navigation, actions, settings changes. |
| `allow_raw_methods` | off | Adds `call_method`, which reaches any host method and bypasses the limits below. |

The server speaks plain HTTP. Do not expose it beyond a trusted network without a TLS reverse proxy.

## Open WebUI

1. Set `bind_address` to an address Open WebUI can reach (`0.0.0.0` for all), set a `token`, and let the
   plugin reload.
2. In Open WebUI, open **Admin Settings → External Tools → Add**, choose type **MCP (Streamable HTTP)**, and
   enter the URL, with auth **Bearer** and the token.
   - Open WebUI in Docker on the same machine: `http://host.docker.internal:8765/mcp` (on Linux add
     `--add-host=host.docker.internal:host-gateway`).
   - Another machine: `http://<rsimgv-host>:8765/mcp`.
3. Allow the port through the firewall if needed.

Open WebUI calls the server from its backend, so the model needs no access to your browser.

## Tools

38 tools, each with MCP annotations (`readOnlyHint`, `destructiveHint`, ...); `TOOLS.md` lists them with their
arguments, and what is deliberately left out.

- **Look:** `get_status` (start here), `view_info`, `view_screenshot` and `get_thumbnails` (images; `format`
  png or jpeg, `quality`, `max_size`, and for screenshots a fractional `crop` and `info` to include `view_info`'s result), `get_file_info`, `get_file_state`, `get_playlist`, `get_marked_files`. `view_info` reports
  state too: playing, volume, mute, repeat, slideshow, rotation, zoom and fit.
- **Browse:** `list_files` (paged, filterable), `search_files` (the search bar's query language), `open_folder`, `navigate`, `step_folder`, `select_file`,
  `open_item`, `close_viewer`, tabs and bookmarks.
- **Viewer:** `viewer_navigate`, `image_view`, `transform_view`, `video_playback`, `video_view`, `slideshow`,
  `playlist_control`, `playlist_toggle_item`.
- **Everything else:** `list_actions` finds an action by text and `run_action` runs it. Actions that open
  dialogs, quit, need a pointer or depend on keyboard focus are refused, and settings under `plugins.` cannot
  be changed.

rsimgv must be running with a window open; calls made without one return an error.
