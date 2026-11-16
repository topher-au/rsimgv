# MCP tool catalogue (working draft)

Design notes for what the MCP Server plugin should expose. Nothing here is implemented beyond the
"current" tools in `plugin.py`; this is the list to agree on before changing them.

Source material: the host methods in `src/services/remote.rs` (about 45), the 153 keybound actions in
`src/keybindings/actions.rs`, and the plugin actions (`au.topher.upscale`, `au.topher.llm-chat`).

## Principles

1. **Few, well-typed tools for what models do often; one discoverable `run_action` for the long tail.**
   Never one tool per action: most actions are keyboard-level UI moves that take no arguments.
2. **Group small variants behind an enum argument** (`direction: back|forward|up`) instead of separate
   tools. Fewer tools means less context and better tool choice for small local models.
3. **Return structured JSON**: `structuredContent` plus the same JSON as text for clients that ignore it.
   Images (screenshots, thumbnails) are image content blocks, never base64 inside text.
4. **Annotate every tool**: `title`, `readOnlyHint`, `destructiveHint`, `idempotentHint`,
   `openWorldHint: false`. `read_only` mode is then "hide every tool without `readOnlyHint`", not a name list.
5. **Absolute paths in, absolute paths out.** File names are accepted only where the host already accepts them.
6. **Bound the output.** Anything that lists files takes `limit` and `offset` and reports `total`.
7. **Errors** are tool results with `isError: true` and the host's message, not JSON-RPC errors.
8. **The MCP client must not be able to change the MCP server's own exposure** (see "Excluded").

## Current tools (27) and what to do with each

| Current tool | Host method | Verdict |
| --- | --- | --- |
| `view_info` | `view.info` | **Keep.** Extend host to report paused, volume, mute, zoom, rotation (see gaps). |
| `view_screenshot` | `view.screenshot` | **Keep.** Now takes format, quality, max_size and crop (done). |
| `window_info` | `window.info` | **Merge** into `get_status`. |
| `selected_file` | `files.selected` | **Merge** into `get_status`. |
| `list_tabs` | `tabs.list` | **Keep.** |
| `open_tab` | `tabs.open` | **Keep.** |
| `activate_tab` | `tabs.activate` | **Keep.** |
| `close_tab` | `tabs.close` | **Keep**; mark destructive. |
| `list_files` | `files.list` | **Change**: add `limit`, `offset`, `query`, `media_only`; return a list, not a name-keyed map. |
| `select_file` | `files.select` | **Merge** with `select_file_index` into one `select_file` taking `file` or `index`. |
| `select_file_index` | `files.select_index` | **Merge** (above). |
| `navigate_to` | `navigate.to` | **Keep**, rename `open_folder`. |
| `navigate_back/forward/up` | `navigate.*` | **Merge** into `navigate {direction, n}`. |
| `next_folder`, `previous_folder`, `next_media_folder`, `previous_media_folder` | `navigate.*` | **Merge** into `step_folder {direction, media_only}`. |
| `file_metadata` | `metadata.get` | **Keep**, cap `paths` at 50. |
| `list_bookmarks` | `bookmarks.list` | **Keep.** |
| `list_actions` | `actions.list` | **Change**: `query`, `scope` filters; drop `icon`/`defaults`; hide excluded actions. |
| `run_action` | `actions.run` | **Keep**, with the exclusion list below and `value`/`target` typed as number/string. |
| `show_message` | `messages.show` | **Keep**, low priority. Lets the model tell the user something on screen. |
| `describe_settings` | `settings.describe` | **Change**: `query` filter; hide excluded keys. |
| `get_setting` | `settings.get` | **Keep.** |
| `set_setting` | `settings.set` | **Keep**, with the excluded-key list. |
| `call_method` | any | **Remove from the default set.** Offer behind an `allow_raw_methods` setting (off by default); it bypasses every other safeguard. |

That takes 27 tools to about 22, with fewer distinct decisions for the model.

## Proposed catalogue

R = read-only, W = changes state, D = destructive.

### Observe

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `get_status` | none | `{folder, selected_file, selected_index, viewer_open, tab:{id,index}, sidebar, item?:{filename, mime_type, width, height, position, duration, fps, speed}}` | One call that orients the model. Replaces `window_info`, `selected_file`, most `view_info` use. Built from `window.info`, `files.selected`, `tabs.current`, `view.info`. | R |
| `view_info` | none | the `view.info` object | Detail on the viewed item. Kept for when `get_status` is more than needed. | R |
| `view_screenshot` | `format?: png\|jpeg\|webp` (default png), `quality?: 1-100` (default 85), `max_size?: int` (longest side, never enlarges), `crop?: {x,y,width,height}` (fractions 0-1 of the full picture, applied before `max_size`) | image block + `{format,width,height,source_width,source_height}` text | See what the viewer shows, or zoom on a region. **Implemented** (host does the work). | R |
| `get_thumbnails` | `paths: string[]` (max 12), `max_size?: int` (default 256, max 2048), `format?`, `quality?` | text `{images:[{path,size,width,height,format}], missing:[path]}` followed by one image block per found file, in that order | Let a model look at a folder without opening each file. Size is the largest cached thumbnail not above `max_size`. **Implemented.** | R |
| `get_file_info` (was `file_metadata`) | `paths: string[]` (max 50) | `{files:[{path,name,is_dir,size,modified,mime_type,media,media_pending}]}` | Size, type, dimensions, codec. `media_pending` means "ask again shortly". | R |
| `get_file_state` | `path` | `{path, hash, values}` | Saved per-file state such as the resume position. | R |

### Browse and select

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `list_files` | `path?`, `query?`, `media_only?`, `limit?` (default 100), `offset?` | `{path, total, files:[{name,path,is_dir,size,modified}]}` | List the current tab's items (in tab order) or a folder from disk. | R |
| `search_files` | `query`, `path?`, `recursive?`, `limit?` | same shape as `list_files` | Find by name or attribute. Needs a host method over `src/search` (see gaps). | R |
| `open_folder` | `path` | `{current_path}` | Browse a folder in the current tab. | W |
| `navigate` | `direction: back\|forward\|up`, `n?` | `{moved, current_path}` | History and parent navigation. | W |
| `step_folder` | `direction: next\|previous`, `media_only?` | `{path}` (null at the end) | Walk the folder tree. | W |
| `select_file` | `file?` (name or path) or `index?` (negative from end) | `{selected, index}` | Move the selection; opens nothing. | W |
| `open_item` | none | `{filename}` | Show the selected picture or video in the viewer. Wrap `browse.open` / `win.toggle-viewer`. | W |
| `close_viewer` | none | `{}` | Return to the browse view. Wrap `view.close`. | W |

### Tabs and bookmarks

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `list_tabs` | none | `{tabs:[{id,index,path,active}]}` | | R |
| `open_tab` | `path`, `activate?` | `{id,index}` | | W |
| `activate_tab` | `index` or `id` | `{}` | | W |
| `close_tab` | `index` | `{}` | | D |
| `list_bookmarks` | none | `{bookmarks:[{name,path}]}` | | R |
| `add_bookmark` | `path`, `name?` | `{}` | Host method exists, not yet exposed. | W |
| `remove_bookmark` | `path` | `{}` | Host method exists, not yet exposed. | D |

### Viewer and playback (typed wrappers over actions)

Enum-grouped so the model does not need `list_actions` for the common things. Each maps to existing actions
and validates `value` against the action's range before calling `actions.run`.

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `viewer_navigate` | `to: previous\|next\|first\|last` | `{filename}` | Step through the folder (or playlist) in the viewer. `view.*` actions. | W |
| `image_view` | `command: zoom_in\|zoom_out\|fit_window\|fit_width\|fit_height\|fill\|original\|pan_up\|pan_down\|pan_left\|pan_right`, `steps?: int` | `{}` | Zoom, fit and pan a picture. | W |
| `transform_view` | `command: rotate_cw\|rotate_ccw\|set_rotation\|flip_h\|flip_v`, `degrees?: 0\|90\|180\|270` | `{}` | Rotation and mirroring for pictures and videos. | W |
| `video_playback` | `command: play_pause\|seek\|frame_step\|set_speed\|repeat\|mute\|volume`, `value?: number`, `target?: video\|application` | `{}` | Seconds for `seek`, frames for `frame_step`, a multiple for `set_speed`, a percent change for `volume`. Add a `paused`/`position` readback once the host reports it. | W |
| `video_view` | `command: zoom_in\|zoom_out\|zoom_reset\|pan_*\|set_aspect\|toggle_crop\|toggle_backdrop`, `value?: string` (aspect such as `16:9`) | `{}` | Framing of a video. | W |
| `slideshow` | `state: start\|stop\|toggle`, `shuffle?` | `{}` | | W |

### Playlist

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `playlist_control` | `command: play\|next\|previous\|first\|last\|shuffle`, | `{}` | `win.playlist-*` navigation. | W |
| `playlist_toggle_item` | none | `{}` | Add or remove the viewed item (or the marked ones). Wrap `win.playlist-toggle`. | W |
| `get_playlist` | none | `{items:[{path}], current, shuffled}` | **Needs a host method.** | R |

### Settings and messages

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `describe_settings` | `query?`, `limit?` | `{settings:[{key,type,description,default,value}]}` | | R |
| `get_setting` | `key` | `{key,value}` | | R |
| `set_setting` | `key`, `value` (null resets) | `{key,value}` | Refuses the excluded keys. | W |
| `show_message` | `text`, `level?`, `seconds?`, `position?` | `{id}` | Put a note on screen for the user. | W |

### Discovery and escape hatch

| Tool | Args | Returns | Purpose | |
| --- | --- | --- | --- | --- |
| `list_actions` | `query?`, `scope?: global\|browse\|view\|image\|video\|playlist\|tree`, `limit?` | `{actions:[{name,label,description,scope,takes_value?,takes_target?}]}` | Find an action. Includes plugin actions. | R |
| `run_action` | `name`, `value?: number`, `target?: string` | `{}` or the plugin's result | Run any non-excluded action. Plugin actions can take up to 120 s (upscale). | W |
| `list_plugins` | none | the `plugins.list` result | See what is loaded. | R |

## Excluded (not exposed over MCP)

### Host methods

| Method | Why |
| --- | --- |
| `settings.list`, `settings.update`, `settings.reset` | `describe_settings` and `set_setting` cover them; bulk writes are hard to review. |
| `plugins.settings`, `plugins.set-setting`, `plugins.reload` | A client could rewrite this plugin's own `bind_address`, `token` or `read_only`, or load other code. |
| `files.set_state`, `files.reset_state` | Niche, and they silently lose a user's resume positions. |
| `tabs.files`, `tabs.select`, `tabs.current` | `list_files`, `select_file` and `get_status` cover the selected tab; revisit if multi-tab work matters. |
| `messages.update`, `messages.dismiss` | Not worth tools; `show_message` has a timeout. |
| `window.set_show_sidebar` | UI chrome. |
| `upscaled.path`, `view.reload` | Internal/plugin plumbing. |
| `call_method` (tool) | Opt-in only, see above. |

### Settings keys

`plugins.*` (disabled list, extra directories, auto-reload): these decide what code runs in the app.

### Actions (hidden from `list_actions`, refused by `run_action`)

| Group | Actions | Why |
| --- | --- | --- |
| Lifecycle | `app.quit` | A remote client should not close the app. |
| Dialogs that wait for a person | `win.settings`, `win.settings-keybindings`, `win.open-folder`, `win.edit-location`, `win.bookmark-folder`, `win.playlist-load`, `win.playlist-save`, `win.playlist-clear`, `browse.properties`, `win.toggle-keybinds` | They block on a human at the desktop. Use `open_folder`, `add_bookmark` and so on. |
| Pointer-only | `image.zoom-in-at-pointer`, `image.zoom-out-at-pointer`, `browse.show-tooltip`, `video.edit-crop` | No pointer exists over MCP. |
| Keyboard cursor and focus | `browse.cursor-*`, `browse.keep-*`, `browse.extend-*`, `browse.first/last`, `browse.select-all`, `browse.toggle-mark`, `tree.*`, `playlist.cursor-*`, `playlist.extend-*`, `playlist.select-all`, `playlist.move-*`, `playlist.*search`, `win.focus-*` | Fragile UI-state moves that depend on focus. `select_file` and `open_folder` are the reliable route. Revisit marking and playlist reordering if a use appears. |
| Layout chrome | `win.toggle-sidebar`, `win.toggle-filesystem`, `win.toggle-bookmarks`, `win.toggle-playlist`, `win.playlist-detach`, `view.toggle-thumbnail-bar*`, `view.pin`, `view.toggle-status-bar`, `win.toggle-fullscreen` | Low value for a model; allow later if wanted. `win.toggle-fullscreen` is useful enough to reconsider. |
| Duplicate | `win.notify` | `show_message` replaces it. |

Everything else (about 90 actions: viewer navigation, zoom/fit/pan, rotate/flip, playback, volume, speed,
aspect, crop toggles, SVP, backdrop, timeline settings, browse display toggles, playlist navigation, plugin
actions) stays reachable through `run_action`.

## Host gaps (Rust changes in `src/services/remote.rs`)

Without these the model can act but cannot confirm the result:

1. **Viewer state readback**: paused, volume, mute, zoom, fit mode, rotation, flip, repeat, slideshow
   running. Add to `view.info`.
2. **Playlist state**: `playlist.get` returning items, current index and shuffle. Perhaps `playlist.add`,
   `playlist.remove`, `playlist.clear` without a dialog.
3. **Marked items** in the browse view: needed before marking actions are worth exposing.
4. **Search**: a `files.search` method over `src/search`, instead of each client re-implementing it (the
   Open WebUI tool does it client-side today).
5. ~~**Screenshot size**~~ Done: `view.screenshot` and `thumbnails.get` take format, quality and size
   (`view.screenshot` also takes a crop); the host encodes with gdk-pixbuf. WebP needs gdk-pixbuf's WebP
   loader, and the call fails with `unsupported_format` where it is missing.
6. **Action metadata**: `actions.list` should say whether an action takes a value or a target, and give
   `value_info`'s range, so `run_action` and `list_actions` can validate and describe them.

## Open questions

- Should `viewer_navigate` etc. exist at all, or is `run_action` plus a good `list_actions` enough? The
  wrappers cost about eight tool definitions; the benefit is reliability on small models.
- Is `show_message` worth keeping, or does it only tempt models to narrate to an empty room?
- Should `read_only` gate by annotation (proposed) or keep the explicit name list?
- Do we want per-tool toggles in the plugin settings, as the Open WebUI tool has valves?
