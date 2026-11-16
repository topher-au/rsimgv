# MCP Server

Serves the [Model Context Protocol](https://modelcontextprotocol.io) from inside rsimgv so an MCP client
can browse folders, drive the viewer, read what it shows and change settings.

The endpoint is `http://127.0.0.1:8765/mcp` (streamable HTTP, JSON responses). It listens on the loopback
interface only and rejects requests whose `Host` or `Origin` is not local. Plugins are trusted code and the
tools can change settings and run any action, so set a token if other local users share the machine.

## Connect Claude Code

```sh
claude mcp add --transport http rsimgv http://127.0.0.1:8765/mcp
```

With a token: add `--header "Authorization: Bearer <token>"`.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `port` | 8765 | Listening port; applies after the plugin reloads. |
| `token` | empty | When set, clients must send it as a Bearer token. |

## Tools

Most tools map to one host method: `view_info`, `view_screenshot` (returned as an image), `list_files`,
`select_file`, `navigate_*`, `open_tab`, `file_metadata`, `show_message`, `get_setting`, `set_setting` and so on.
`list_actions` and `run_action` reach every keybound action, such as zoom and playback, and `call_method`
calls any method listed in `src/services/remote.rs`.

rsimgv must be running with a window open; calls made without one return an error.
