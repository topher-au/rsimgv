# LLM Screenshot — Python

Sends the viewer's current picture or video frame to an OpenAI-compatible
vision model and shows the description in an overlay notification and a sidebar
panel.

## Requirements

- A build of rsimgv with the `python` plugin feature enabled.
- PyGObject with GTK 4 support (`python3-gobject` on most distributions).
- An API key in an environment variable, `OPENAI_API_KEY` by default.

## Install

The plugin is installed with rsimgv. To install it by hand, copy this directory
to a plugin search path:

```sh
mkdir -p ~/.local/share/rsimgv/plugins
cp -r llm-screenshot ~/.local/share/rsimgv/plugins/com.example.llm-screenshot
```

## Configure

Use **Preferences → Integration → Plugins → LLM Screenshot** to set the
endpoint, model, prompt, token limit, timeout and the environment variable that
holds the API key. The values are saved in
`~/.config/rsimgv/plugins/com.example.llm-screenshot/config.json`.

The API key is read from the environment variable named by `api_key_env`,
`OPENAI_API_KEY` by default. It is never written to the config file.

`config.example.json` is a starting point.

## Use

Open a picture or video in the viewer and press `Ctrl+Alt+L`, or choose
**Analyze with LLM** from the main menu. The description appears in a
persistent overlay notification and in the **LLM Response** panel in the
sidebar. Errors, missing keys, and timeouts appear as window notifications.

When the action is invoked through the JSON IPC (`actions.run`), the call waits
for the model's reply and returns it:

```json
{"id": 1, "result": {"result": "The picture shows ..."}}
```

An error is returned as `{"code": "plugin_error", "message": "..."}`.

## Behavior

- The screenshot is captured on the GTK main thread through
  `rsimgv.host.call("view.screenshot")`.
- The HTTP request runs on a daemon thread, so the window stays responsive.
- The reply is marshalled back with `rsimgv.ui.idle_add` and reported to the
  host with `rsimgv.action_done`, which completes a waiting IPC call.
- A reload bumps `rsimgv.host.generation`; late replies from an old generation
  are ignored.
- Only one request runs at a time.
- The API key is read from the environment and never logged or saved.
- Full-size screenshots are sent. A screenshot whose base64 body exceeds
  roughly 20 MB is refused before the request is made.

## Current limitations

- No streaming; the full reply is shown when the request completes.
- No downscaling. A future `max_size` parameter on `view.screenshot` or a
  GdkPixbuf resize would reduce request size.
- C extensions imported by the plugin and GObject subclasses defined by it are
  subject to the hot-reload limits documented in `docs/PLUGINS.md`.
