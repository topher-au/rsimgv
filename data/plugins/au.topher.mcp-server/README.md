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
| `read_only` | off | Hides the tools that navigate, run actions or change settings. |

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

Most tools map to one host method: `view_info`, `view_screenshot` (returned as an image), `list_files`,
`select_file`, `navigate_*`, `open_tab`, `file_metadata`, `show_message`, `get_setting`, `set_setting` and so on.
`list_actions` and `run_action` reach every keybound action, such as zoom and playback, and `call_method`
calls any method listed in `src/services/remote.rs`.

rsimgv must be running with a window open; calls made without one return an error.
