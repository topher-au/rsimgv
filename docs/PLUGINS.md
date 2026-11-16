# Plugins

rsimgv can load Python plugins that add actions, keybindings, menus, events and sidebar panels. The plugin
system is optional at build time (`-Dpython=auto|enabled|disabled`) and is enabled by default when
`python3-embed` and `pygobject-3.0` are found.

The bundled **LLM Screenshot** plugin is installed with the application. It sends the viewer's current
picture or video frame to an OpenAI-compatible vision model and shows the description in an overlay
notification. Configure it on **Preferences → Integration → Plugins**.

## Writing your first plugin

This builds a plugin that shows a notification naming the picture or video in the viewer, and logs each item
the viewer changes to. The reference sections below explain each part.

1. **Make a plugin directory** and point rsimgv at its parent while you develop:

   ```sh
   mkdir -p ~/dev/plugins/com.example.hello
   export RSIMGV_PLUGINS=~/dev/plugins
   ```

2. **Write the manifest**, `~/dev/plugins/com.example.hello/plugin.yaml`:

   ```yaml
   api: 1
   id: com.example.hello
   name: Hello
   version: 0.1.0
   runtime: python
   entry: plugin.py
   startup: on-demand

   events: [viewer.item_changed]

   contributes:
     actions:
       - id: greet
         label: Greet
         description: Say which item the viewer shows.
         scope: view
         default_keys: [ctrl+alt+g]
   ```

   `scope` says where the key works: `global`, `browse`, `view`, `image`, `video`, `playlist` or `tree`.

3. **Write the plugin**, `~/dev/plugins/com.example.hello/plugin.py`:

   ```python
   import rsimgv


   class Plugin:
       def activate(self, host):
           rsimgv.host.log("info", "Hello plugin active")

       def deactivate(self):
           pass

       def invoke_action(self, action, context):
           if action != "greet":
               return
           try:
               info = rsimgv.host.call("view.info")
           except Exception as exc:
               rsimgv.notify(f"Hello: {exc}", level="error")
               return
           rsimgv.notify(f"Viewing {info.get('filename') or 'nothing'}")

       def on_event(self, name, data):
           if name == "viewer.item_changed":
               rsimgv.host.log("info", f"Viewer moved to {data['path']}")
   ```

4. **Run it.** Start `rsimgv` from the shell where you set `RSIMGV_PLUGINS`, with `--verbose` to see the log.
   Open **Preferences → Integration → Plugins**: Hello is listed there. Open a picture and press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>G</kbd>. The key can be changed in the
   keybinding editor, where the action is named `plugin.com.example.hello.greet`.

5. **Edit and reload.** Change the notification text and save. The plugin reloads on its own (unless
   `plugins.auto-reload` is off), or send `plugins.reload` over the remote control server. A plugin action
   that raises an error is reported as a window notification.

From here:

- `rsimgv.host.call(method, params)` runs the same methods as the remote control server, such as `view.info`,
  `view.screenshot`, `files.list` and `settings.get`. The methods are listed at the top of `src/services/remote.rs`.
- Add `contributes.settings` to the manifest for options. Read them from the `config.json` beside
  `rsimgv.host.config_path()`, as the bundled plugins do.
- Add `contributes.panels` and `create_panel` for a widget in the sidebar or over the viewer.
- Do slow work, such as network requests, on a thread, and return to the GTK main thread with
  `rsimgv.ui.idle_add`. A plugin that blocks stalls the whole window.
- Use `rsimgv.action_done(result)` if a script driving the app over IPC should wait for the action's result.
- The bundled plugins in `data/plugins/` are complete examples: LLM Screenshot is the smallest.

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
- Panel slots: `sidebar.bottom`, `browse.sidebar`, `browse.header`, `viewer.overlay`, `preferences.plugins`. Only
  `sidebar.bottom`, `browse.sidebar` and `viewer.overlay` are built so far. A `browse.sidebar` panel is in a
  sidebar on the right of the browse view, outside the playlist's, whose width can be dragged. The sidebar is
  open while a panel in it is shown, so a plugin shows or hides its panel with `container.set_visible`. A `viewer.overlay` panel is a card over the viewer's picture, in the
  corner given by `position` (`top-left`, `top-right`, `bottom-left` or `bottom-right`; `top-right` by default).
  The plugin sizes it and can hide it with `container.set_visible`.
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

## Bundled LLM Chat plugin

`au.topher.llm-chat` adds a persistent chat to the sidebar, and over the picture in the viewer (`Ctrl+Alt+C` shows or hides it). Each message is sent to an OpenAI-compatible
vision model with the viewer's current picture or frame, its filename and its metadata. Replies are shown as markdown with the model's reasoning, and can be read aloud. The model can use tools to keep
memories and to control the viewer (zoom, next and previous, play and pause). Voice input and
spoken replies can use an HTTP endpoint or a local program such as `whisper-cli` or `piper`, and a hands-free
voice conversation listens for speech, sends it and speaks the reply (`Ctrl+Alt+V`). See the
plugin's README for settings.

## Limits

- Hot reload re-imports Python modules; C extensions and GObject types from earlier generations stay
  loaded for the process lifetime.
- A plugin stuck in a loop blocks the GTK thread.
- Native Rust plugins are not implemented yet.
