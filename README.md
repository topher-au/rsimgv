<div align="center">

# rsimgv

**An integrated file explorer and media viewer for GNOME.**

Written in Rust with GTK 4 and libadwaita.

<p>
  <a href="https://www.repostatus.org/#wip"><img src="https://www.repostatus.org/badges/latest/wip.svg" alt="Project status: work in progress"></a>
  <a href="https://www.gnu.org/licenses/gpl-3.0.html"><img src="https://img.shields.io/badge/License-GPL--3.0--or--later-blue?style=for-the-badge" alt="License: GPL-3.0-or-later"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-stable-b7410e?style=for-the-badge&logo=rust&logoColor=white" alt="Rust"></a>
  <a href="https://www.gtk.org"><img src="https://img.shields.io/badge/GTK-4.22+-4a86cf?style=for-the-badge&logo=gtk&logoColor=white" alt="GTK 4.22+"></a>
  <a href="https://gnome.pages.gitlab.gnome.org/libadwaita/"><img src="https://img.shields.io/badge/libadwaita-1.9+-3584e4?style=for-the-badge" alt="libadwaita 1.9+"></a>
  <a href="https://www.kernel.org"><img src="https://img.shields.io/badge/Linux-platform-fcc624?style=for-the-badge&logo=linux&logoColor=black" alt="Linux"></a>
</p>

</div>

rsimgv puts a folder tree, a browse grid and a navigation history side by side, so you can move through a
library of pictures and videos without leaving the window.

> **Status:** early development. The file explorer, media viewer and thumbnails are implemented, but the
> project is still changing quickly.

![rsimgv showing the folder tree, the home folder, and the navigation and path bar](docs/screenshots/main-window.png)

## Contents

- [Features](#features)
- [Requirements](#requirements)
- [Building and running](#building-and-running)
- [Command line](#command-line)
- [Settings](#settings)
- [Plugins](#plugins)
- [Project layout](#project-layout)
- [Application ID](#application-id)
- [License](#license)

## Features

- **Folder tree** rooted at `/` or the home folder, picked by the buttons on its section header. Going to
  a folder outside the root returns the tree to `/`. Folders expand on demand, and selecting one reveals
  its path. Expansion can keep other folders open or collapse them, and a path is framed so the folder and
  as many of its children as fit are on screen.
- **Bookmarks** in a collapsible section above the tree. The bookmark of the folder shown is highlighted.
  Drag a bookmark to reorder it, middle-click it to open a new tab, or right-click it to rename or remove it.
- **Browse view** showing the current folder as a grid of files and folders. Folders come first, then
  files, in natural name order. Item size is adjustable, names can be ellipsized or wrapped, and the info
  line shows size and modification time.
- **Selecting items**: a click selects one item. Ctrl-click adds or removes one item, and Shift-click marks the items from the anchor, the item the cursor last moved from by a plain click or move, to the one clicked. The arrow and Page keys move the cursor and select only the item it lands on. With Ctrl they move the cursor and keep the selection, with Shift they select every item from the anchor to the cursor, and Ctrl+Space adds or removes the item under the cursor.
- **Media filter**: only pictures and videos are listed by default. Turn it off to see every file.
- **Video previews**: while a video is the active item, its preview can play a sequence of captured frames or an embedded
  player in the item's frame. The preview follows the video's locked crop, and fills the square when thumbnails are cropped
  that way.
- **Search** in the browse view: the search button in the header, or Ctrl+Shift+F, slides a bar down under the header. It filters the folder's items by name, by any attribute, or by one attribute such as size, modified time or type. With Subfolders on, the bar searches the whole folder tree under the folder shown, and the folders' matches are listed together.
- **Path bar** in the header with clickable breadcrumbs. It starts at home when you are inside it, and at
  `/` otherwise. Middle-click or Ctrl-click opens a new tab or window (currently both navigate in place).
- **Navigation** with back, forward and up buttons. Long-pressing back or forward lists the history in
  that direction.
- **Manual location entry**: click the current folder in the path bar to type a path. A leading `~` means
  your home folder.
- **Settings** in a preferences dialog, saved as YAML and applied immediately. Metadata and the viewing history are kept in `media-index.sqlite3` in the user's data directory (`RSIMGV_DATA` overrides its folder).
- **Hidden files** can be shown from the main menu.
- **Viewing history**: how many times each picture or video has been viewed, and how long it has been watched, measured by an in-app timer so faster or slower playback counts the time actually spent. The viewer's status bar and overlays show the totals, the Viewing History item in the main menu lists recent files, and the History preferences choose what counts as a view (five seconds for a picture, and for a video either twenty seconds of it or a tenth of its length, by default). The counts can also be shown in the browse view's item info.
- **Playlist**: one list of pictures and videos from anywhere, in your own order, kept between runs and shared by the windows. Playing it leaves the folder you are browsing, and the item selected there, as they were. See [Playlist](#playlist).
- **Scripts** are commands listed on the Scripts tab of the Integration page, with `%file%` standing for the item they run on. The Run Script action in the keybinding editor binds a key to one: it runs on the item under the cursor, or on the one the viewer shows. A script that waits to finish has the folder listed again when it ends.
- **Plugins** extend the app with Python actions, keybindings, menus, events and sidebar panels. They are discovered from the XDG data directories, reload when their files change, and have their settings on the Plugins page. The bundled **LLM Screenshot** plugin sends the viewer's current picture or video frame to an OpenAI-compatible vision model and shows the description in an overlay notification. See [Plugins](#plugins).
- **Messages** appear over the window: notifications that go away by themselves, and persistent messages that stay until dismissed. A script that fails reports it this way, and other programs can show their own through the remote control server (`messages.show`). F1 shows a list of the keybindings in force where you are, and hides it again. Skipping through a video shows a progress bar and the time over it, which the Overlay tab of the Viewer page sets up. With SVP on, skipping leaves it out until a moment after the last skip, and a new video starts without it, so neither waits for SVP to load; the SVP tab of the Videos page sets how long that is, or turns it off. V turns SVP on or off while watching. That tab also takes the folder of SVP's own plugin libraries, which replace the bundled open-svpflow ones. The Show Notification action in the keybinding editor binds a key to a notification: each binding sets its message, level and how many seconds it stays.

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
./dev.sh              # rebuild and run rsimgv
./dev.sh test         # rebuild and run the test suite
./dev.sh cargo check  # run a Cargo command in the build environment
```

Tests need a display. On a machine without one, run them under `xvfb-run -a`.

To build and install with Meson:

```sh
meson setup builddir --prefix=/usr/local
meson compile -C builddir
meson install -C builddir
```

Installing copies the GSettings schema and the application icons into the system data directories. The
program embeds its user interface, stylesheets and icons in a GResource bundle, so it does not need them
installed separately.

## Command line

```sh
rsimgv [FILE_OR_FOLDER...]
```

Each argument opens in a new tab of the open window, or of a new window if none is open. A folder opens
in its tab. A file opens in a tab on its folder with the file selected, and a picture or video also opens
in the viewer. A running rsimgv takes the arguments of a second launch. `rsimgv --version` prints the version,
and `rsimgv --verbose` prints the debug output.

## Playlist

Items in the playlist have a line around them in the browse view. To add or remove them:

- press <kbd>I</kbd> on the item under the cursor, on the marked items, or in the viewer on the item it shows; pressing it again takes them off
- right-click an item or folder and choose **Add to Playlist** or **Remove from Playlist**
- drag files onto the playlist panel, or use the remove button on a row there

A folder stands for the pictures and videos in it and in the folders under it. A file appears once, however many times it is added. Rows in the sidebar can be dragged into another order, and double-clicking one plays from it.

| Key | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | Show the playlist in the viewer, from the item it was left on |
| <kbd>]</kbd> and <kbd>[</kbd> | Next and previous item, going round the ends. Pressed elsewhere, they show the playlist from the item it was left on |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>P</kbd> | Show or hide the playlist panel, on the right opposite the sidebar |

These can all be rebound, and first item, last item, load, save and clear can be given keys, in the keybinding editor. While the viewer shows the playlist, its own previous and next keys, the thumbnail bar and auto-advance go through the playlist too. They stop at its ends, whatever the **folder-end** setting says.

The panel's buttons load and save M3U and M3U8 (`.m3u`, `.m3u8`), PLS (`.pls`) and XSPF (`.xspf`) files, and XSPF is the default when a name has no extension. Saved files use absolute paths. Each item keeps its title, and its duration where it is known, in whatever the format holds. Loading into a playlist that has items asks whether to replace them or add to the end. Web addresses and files that are not pictures or videos are left out, and you are told how many. A file listed twice is kept once.

The shuffle button plays the playlist in a random order, with the item shown first so it keeps playing. The order the items were made in is kept beside it: turning shuffle off goes back to that order, and moving items while shuffled changes only the shuffled order. Adding and removing change both. Saved playlist files keep the made order, the shuffled order too, which items have been played, and the item shown, in comments and extensions that other players ignore. Exporting a file keeps as much of that state as the format holds: XSPF lists the made order with each item's place in the shuffle, and the other formats write the order the panel shows.

The panel's detach button takes it out into a window of its own, which keeps working with the playlist and its keys. Its button puts it back, and closing that window does the same. The panel shows beside the browse view on the right, opposite the sidebar.

The playlist is kept as XSPF in `playlist.xspf` beside the settings file; an older `playlist.m3u8` is read and moved over the first time the playlist is loaded. Other programs can drive it with the `win.playlist-*` actions over the remote control server.

## Settings

Settings are stored as YAML in `~/.config/rsimgv/rsimgv.conf`. Set `RSIMGV_CONFIG` to use another file.

The file is nested by each setting's GSettings key path below the schema's path, `/au/topher/rsimgv/`. Each setting holds
its plain value, and its type comes from the schema:

```yaml
window:
  height: 640
  sidebar:
    width: -1
browse:
  info:
    fields:
      - size
      - modified
```

A value that does not fit its setting's type is kept in the file as written and ignored, and the setting uses its default.

Settings are read and written through GSettings, using a backend the application creates and passes to
`gio::Settings`. Changes made in the preferences dialog, from the menu, or by editing the file while the app runs all
take effect straight away. A file that cannot be parsed makes the settings read-only until it is fixed and the app is
restarted, so it is never overwritten. The schema is `data/au.topher.rsimgv.gschema.xml`.

Each group is a child schema in the schema file, such as `au.topher.rsimgv.window.sidebar` at
`/au/topher/rsimgv/window/sidebar/`. GSettings key names cannot contain `.`, so code names a setting by its dotted path
through `settings::Settings`, as in `settings.int("window.sidebar.width")`, and that picks the child schema to use.

## Plugins

Python plugins can add actions, keybindings, menus, events and sidebar panels. They live in
`~/.local/share/rsimgv/plugins` or any `$XDG_DATA_DIRS/rsimgv/plugins`; `RSIMGV_PLUGINS` overrides the
search path while developing. The Plugins page under Integration lists them, exposes the settings each
plugin declares, and reloads them. Python files and manifests are watched and reloaded when they change.
See [docs/PLUGINS.md](docs/PLUGINS.md) for the manifest, the Python API and the IPC behavior.

The bundled **LLM Screenshot** plugin sends the viewer's current picture or video frame to an
OpenAI-compatible vision model. Open a picture or video and press `Ctrl+Alt+L`, or choose **Analyze with
LLM** from the main menu. The description is shown in an overlay notification and in the sidebar panel.
Set the endpoint, model and prompt on the Plugins page; the API key is read from the environment variable
named there (by default `OPENAI_API_KEY`).

Python plugin support is built by default when `python3-embed` and `pygobject-3.0` are available. Configure
with `-Dpython=auto`, `-Dpython=enabled` or `-Dpython=disabled`.

## Project layout

```
src/
  main.rs            application startup, actions, icon theme registration
  fs/                directory entries, lazy tree nodes, natural sort, folder sorting, thumbnails
  keybindings/       keybinding table, chords and actions
  media/             picture orientation and resampling
  mpv/               libmpv player, video framing, and SVP frame interpolation
  overlay/           on-screen overlay templates
  playlist/          the playlist and its file formats
  services/          IPC, the remote control server and user scripts
  settings/          GSettings settings and YAML storage (store.rs)
  storage/           atomic file writes, bookmarks, per-file settings, the metadata and viewing-history database
  ui/                widgets: window, folder tree, browse view, path bar,
                     navigation bar, header bar, preferences, split view
native/              GObject subclass for GSettings (backend.c)
data/
  ui/                GTK XML templates for every window and widget
  style/             SCSS, compiled to CSS at build time
  icons/             application and symbolic icons (hicolor theme layout)
  *.gschema.xml      GSettings schema
  rsimgv.gresource.xml  bundle manifest
build-aux/           build helper that runs cargo from Meson
```

The GObject subclass that connects GSettings to the YAML file is written in C (`native/backend.c`),
because the Rust bindings do not expose the trait needed to subclass `GSettingsBackend`.

## Application ID

The application ID is `au.topher.rsimgv`. The icons are currently named `au.topher.rsimgv`, so the app icon
will not resolve until the two agree.

## License

GPL-3.0-or-later.
