# Plugins

rsimgv can load Python plugins that add actions, keybindings, menus, events and sidebar panels. The plugin
system is optional at build time (`-Dpython=auto|enabled|disabled`) and is enabled by default when
`python3-embed` and `pygobject-3.0` are found.

The bundled **LLM Screenshot** plugin is installed with the application. It sends the viewer's current
picture or video frame to an OpenAI-compatible vision model and shows the description in an overlay
notification. Configure it on **Preferences → Integration → Plugins**.

## Discovery

Plugins are directories containing `plugin.yaml`. They are looked for in:

1. `$RSIMGV_PLUGINS` (a colon-separated list; useful for development)
2. `$XDG_DATA_HOME/rsimgv/plugins` (usually `~/.local/share/rsimgv/plugins`)
3. each `$XDG_DATA_DIRS/rsimgv/plugins`
4. directories in the `plugins.extra-directories` setting

A plugin is enabled unless its id is in `plugins.disabled`. The switch on each row of the Plugins page turns a
plugin on or off at once and saves the choice there: a disabled plugin's code is stopped and its panels and menu
items are removed, and its keybindings report that it is disabled. Python plugin files and manifests are watched
and reloaded when they change, unless `plugins.auto-reload` is off.

## Manifest

```yaml
api: 1
id: com.example.hello
name: Hello
version: 0.1.0
runtime: python
entry: plugin.py
startup: on-demand

events: [folder.navigated, viewer.item_changed]

contributes:
  actions:
    - id: greet
      label: Greet
      description: Say hello to the selected item.
      scope: browse
      default_keys: [ctrl+alt+g]
  menus:
    - menu: main
      label: Greet
      action: greet
  panels:
    - id: response
      slot: sidebar.bottom
      title: Hello
  settings:
    - key: greeting
      label: Greeting
      kind: string
      default: Hello
```

- `api` must be `1`.
- Plugin ids may contain lowercase letters, digits, dots and hyphens.
- Action names become `plugin.<id>.<action>` and can be bound in the keybinding editor.
- Panel slots: `sidebar.bottom`, `browse.header`, `viewer.overlay`, `preferences.plugins`.
- Setting kinds: `string`, `text`, `int`, `bool`, `choice`. Settings are written to the plugin's own
  `config.json` and shown on the Plugins page. `choice` settings also take `choices`; `int` settings can
  take `min` and `max`.

## Python contract

The host imports `entry` and instantiates `create_plugin()` or the `Plugin` class. It then calls:

```python
class Plugin:
    def activate(self, host): ...
    def deactivate(self): ...
    def invoke_action(self, action, context): ...
    def on_event(self, name, data): ...
    def create_panel(self, panel_id, instance_id, container, context): ...
    def panel_destroyed(self, panel_id, instance_id): ...
```

`create_panel` receives a host-owned `Gtk.Box` and appends its widgets to it.

## `rsimgv` module

```python
rsimgv.host.call(method, params=None) -> dict
rsimgv.host.log(level, message)
rsimgv.host.config_path() -> str
rsimgv.host.plugin_id
rsimgv.host.generation

rsimgv.ui.connect(widget, signal, callback) -> int
rsimgv.ui.idle_add(callback) -> int
rsimgv.ui.timeout_add(ms, callback) -> int
rsimgv.ui.source_remove(id)
rsimgv.ui.disconnect_all()

rsimgv.notify(text, level="info", seconds=4)
rsimgv.request_reload()
rsimgv.action_done(result)
```

- `host.call` runs the same methods as the JSON IPC, on the GTK main thread.
- `ui.idle_add` is safe to call from a worker thread and runs the callback on the GTK main thread.
- `ui.connect` and `ui.timeout_add` are tracked and cleaned up on reload.
- `action_done` reports an action's result. An IPC `actions.run` waits for it and returns the result to the
  client (up to 120 seconds); a dictionary with an `error` key becomes an error response.
- Plugins are trusted code. A C-extension crash can terminate rsimgv.

## IPC

`actions.run` with a plugin action blocks until the plugin calls `rsimgv.action_done` and returns its
result:

```json
{"id": 1, "method": "actions.run", "params": {"name": "plugin.com.example.llm-screenshot.analyze"}}
{"id": 1, "result": {"result": "The picture shows ..."}}
```

`plugins.list` and `plugins.reload` are also available. A plugin action started from a keybinding or menu
does not wait; it reports errors as window notifications.

## Bundled LLM Screenshot plugin

Configuration is on the Plugins page or in
`$XDG_CONFIG_HOME/rsimgv/plugins/com.example.llm-screenshot/config.json`:

```json
{
  "base_url": "https://api.openai.com/v1",
  "model": "gpt-4o-mini",
  "prompt": "Describe this image and identify anything notable about it.",
  "max_tokens": 500,
  "timeout_seconds": 60,
  "api_key_env": "OPENAI_API_KEY"
}
```

The API key is read from the environment variable named by `api_key_env`; it is never stored in the config
file. Open a picture or video in the viewer and press `Ctrl+Alt+L`, or choose **Analyze with LLM** from the
main menu. The description appears in a persistent overlay notification and the sidebar panel.

## Bundled AI Upscale plugin

`au.topher.upscale` runs an ncnn/Vulkan upscaler (`upscayl-bin` or `realesrgan-ncnn-vulkan`) on the viewed or
selected picture and keeps the result in rsimgv's data folder, where the viewer shows it in place of the
original. Press `Ctrl+Alt+U` or choose **Upscale picture** from the main menu, and `U` in the viewer to switch
between the original and the upscaled copy, or `Shift+U` to compare them either side of a draggable divider. See the plugin's README for requirements and settings.

## Limits

- Hot reload re-imports Python modules; C extensions and GObject types from earlier generations stay
  loaded for the process lifetime.
- A plugin stuck in a loop blocks the GTK thread.
- Native Rust plugins are not implemented yet.
