# rsimgv

An integrated file explorer and media viewer for GNOME, written in Rust with GTK 4 and libadwaita.

rsimgv puts a folder tree, a browse grid, and a navigation history side by side, so you can move through a
library of pictures and videos without leaving the window. The media viewer is not built yet.

> **Status:** early development. The file explorer works; the media viewer, thumbnails, and file
> operations are still to come.

## Screenshots

![rsimgv showing the folder tree, the home folder, and the navigation and path bar](docs/screenshots/main-window.png)

## Features

- **Folder tree** rooted at `/`. Folders expand on demand, and selecting one reveals its path. Expansion
  can keep other folders open or collapse them, and a path is framed so the folder and as many of its
  children as fit are on screen.
- **Browse view** showing the current folder as a grid of files and folders. Folders come first, then
  files, in natural name order. Item size is adjustable, names can be ellipsized or wrapped, and the info
  line shows size and modification time.
- **Media filter**: only pictures and videos are listed by default. Turn it off to see every file.
- **Path bar** in the header with clickable breadcrumbs. It starts at home when you are inside it, and at
  `/` otherwise. Middle-click or Ctrl-click opens a new tab or window (currently both navigate in place).
- **Navigation** with back, forward, and up buttons. Long-pressing back or forward lists the history in
  that direction.
- **Manual location entry**: click the current folder in the path bar to type a path. A leading `~` means
  your home folder.
- **Settings** in a preferences dialog, saved as YAML and applied immediately.
- **Hidden files** can be shown from the main menu.
- **Playlist**: one list of pictures and videos from anywhere, in your own order, kept between runs and shared by the windows. Playing it leaves the folder you are browsing, and the item selected there, as they were. See [Playlist](#playlist).
- **Scripts** are commands listed on the Scripts page, with `%file%` standing for the item they run on. The Run Script action in the keybinding editor binds a key to one: it runs on the item under the cursor, or on the one the viewer shows. A script that waits to finish has the folder listed again when it ends.
- **Messages** appear over the window: notifications that go away by themselves, and persistent messages that stay until dismissed. A script that fails reports it this way, and other programs can show their own through the remote control server (`messages.show`). F1 shows a list of the keybindings in force where you are, and hides it again. Skipping through a video shows a progress bar and the time over it, which the Overlay tab of the video preferences sets up. The Show Notification action in the keybinding editor binds a key to a notification: each binding sets its message, level and how many seconds it stays.

## Requirements

Build-time:

- Rust (stable) with `cargo`
- A C compiler and `pkg-config`
- Meson 1.2 or newer, and Ninja
- GTK 4.22 or newer, with development headers (`gtk4`)
- libadwaita 1.9 or newer (`libadwaita-1`)
- OpenCV 4.5 or newer (`opencv`), with Clang (`clang`) for the Rust bindings to generate against it
- `sassc`, to compile the stylesheets
- `glib-compile-resources` and `glib-compile-schemas` (part of GLib)

## Building and running

For development, `dev.sh` sets up the build directory if needed, rebuilds, and runs the app from it:

```sh
./dev.sh          # rebuild and run rsimgv
./dev.sh test     # rebuild and run the test suite
```

Tests need a display; on a machine without one, run them under `xvfb-run -a`.

To build and install with Meson:

```sh
meson setup builddir --prefix=/usr/local
meson compile -C builddir
meson install -C builddir
```

Installing copies the GSettings schema and the application icons into the system data directories. The
program itself embeds its user interface, stylesheets, and icons in a GResource bundle, so it does not need
them installed separately.

## Command line

```sh
rsimgv [FILE_OR_FOLDER...]
```

Each argument opens in a new tab of the open window, or of a new window if none is open. A folder opens
in its tab. A file opens in a tab on its folder with the file selected, and a picture or video also opens
in the viewer. A running rsimgv takes the arguments of a second launch. `rsimgv --version` prints the version.

## Playlist

Items in the playlist have a line around them in the browse view. To add or remove them:

- press <kbd>I</kbd> on the item under the cursor, on the marked items, or in the viewer on the item it shows; pressing it again takes them off
- right-click an item or folder and choose **Add to Playlist** or **Remove from Playlist**
- drag files onto the **Playlist** section of the sidebar, or use the remove button on a row there

A folder stands for the pictures and videos in it and in the folders under it. A file appears once, however many times it is added. Rows in the sidebar can be dragged into another order, and double-clicking one plays from it.

| Key | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | Show the playlist in the viewer, from the item it was left on |
| <kbd>]</kbd> and <kbd>[</kbd> | Next and previous item, going round the ends. Pressed elsewhere, they show the playlist from the item it was left on |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>P</kbd> | Show or hide the list in the sidebar |

These can all be rebound, and first item, last item, load, save and clear can be given keys, in the keybinding editor. While the viewer shows the playlist, its own previous and next keys, the thumbnail bar and auto-advance go through the playlist too. They stop at its ends, whatever the **folder-end** setting says.

The sidebar's buttons load and save M3U and M3U8 (`.m3u`, `.m3u8`), PLS (`.pls`) and XSPF (`.xspf`) files. Saved files use absolute paths. Loading into a playlist that has items asks whether to replace them or add to the end. Web addresses and files that are not pictures or videos are left out, and you are told how many. A file listed twice is kept once.

The playlist is kept in `playlist.m3u8` beside the settings file. Other programs can drive it with the `win.playlist-*` actions over the remote control server.

## Settings

Settings are stored as YAML in `~/.config/rsimgv/rsimgv.conf`. Set `RSIMGV_CONFIG` to use another file.

```yaml
window:
  width: 960
  height: 640
  maximized: false
browse:
  show-hidden: false
  media-only: true
  item-size: 128
  filename-display: ellipsized
  show-directories: first-last
  info-fields: [size, modified]
tree:
  collapse-on-navigate: false
navigation:
  folder-end: stop
  remember-folder-sort: false
viewer:
  image-preloading: true
image:
  fit: window
  keep-fit: true
  original-focus: center
  unlock-minimum-zoom: false
  scaling-filter: linear
thumbnails:
  threads: 4
  cache: true
  unload-offscreen: true
```

They are read and written through GSettings, using a backend that stores them in that file, so changes
made in the preferences dialog, from the menu, or by editing the file while the app runs all take effect
straight away. The schema is `data/au.topher.rimgv.gschema.xml`.

## Project layout

```
src/
  main.rs            application startup, actions, icon theme registration
  fs/                directory entries, lazy tree nodes, natural sort
  playlist/          the playlist and its file formats
  settings/          GSettings backend (backend.c) and YAML storage (store.rs)
  ui/                widgets: window, folder tree, browse view, path bar,
                     navigation bar, header bar, preferences, split view
data/
  ui/                GTK XML templates for every window and widget
  style/             SCSS, compiled to CSS at build time
  icons/             application and symbolic icons (hicolor theme layout)
  *.gschema.xml      GSettings schema
  rsimgv.gresource.xml  bundle manifest
build-aux/           build helper that runs cargo from Meson
```

The GObject subclass that connects GSettings to the YAML file is written in C (`src/settings/backend.c`),
because the Rust bindings do not expose the trait needed to subclass `GSettingsBackend`.

## Application ID

The application ID is `au.topher.rimgv`. The icons are currently named `au.topher.rsimgv`, so the app icon
will not resolve until the two agree.

## License

GPL-3.0-or-later.
