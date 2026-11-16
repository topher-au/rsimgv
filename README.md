<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logo/logo-dark.svg">
  <img src="docs/logo/logo-light.svg" alt="rsimgv" height="96">
</picture>

**An integrated file explorer and media viewer for GNOME.**

Written in Rust with GTK 4 and libadwaita.

[Website](https://topher-au.github.io/rsimgv/) · [Documentation](https://topher-au.github.io/rsimgv/docs/index.html) · [Releases](https://github.com/topher-au/rsimgv/releases) · [Plugins](docs/PLUGINS.md)

[![Version](https://img.shields.io/github/v/release/topher-au/rsimgv?logo=github&style=for-the-badge&labelColor=24292f&color=0969da)](https://github.com/topher-au/rsimgv/releases/latest)
[![License](https://img.shields.io/badge/License-GPL--3.0--or--later-0969da?style=for-the-badge&labelColor=24292f)](https://github.com/topher-au/rsimgv/blob/main/LICENSE)
[![Status](https://img.shields.io/badge/Status-WIP-bf8700?style=for-the-badge&labelColor=24292f)](https://www.repostatus.org/#wip)

[![Rust](https://img.shields.io/badge/Rust-stable-b7410e?style=for-the-badge&logo=rust&logoColor=white&labelColor=24292f)](https://www.rust-lang.org)
[![GTK](https://img.shields.io/badge/GTK-4.22%2B-4a86cf?style=for-the-badge&logo=gtk&logoColor=white&labelColor=24292f)](https://www.gtk.org)
[![libadwaita](https://img.shields.io/badge/libadwaita-1.9%2B-3584e4?style=for-the-badge&logo=gnome&logoColor=white&labelColor=24292f)](https://gnome.pages.gitlab.gnome.org/libadwaita/)
[![Linux](https://img.shields.io/badge/Linux-platform-fcc624?style=for-the-badge&logo=linux&logoColor=black&labelColor=24292f)](https://www.kernel.org)
</div>

rsimgv puts a folder tree, a thumbnail grid, a full picture and video viewer and a playlist in one window, so you
can move through a large library of media without leaving it. Everything is an action you can bind to a key,
script, or drive from another program.

> **Status:** early development (version 0.1.0). Browsing, viewing, playlists, plugins and remote control work
> today. General file operations such as copy and move are not built yet, and the project is still changing quickly.

![rsimgv showing the folder tree, the home folder, and the navigation and path bar](docs/screenshots/main-window.png)

## Contents

- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Plugins and automation](#plugins-and-automation)
- [Development](#development)
- [Project layout](#project-layout)
- [License](#license)

## Features

**Browse**

- A folder tree rooted at `/` or home, with bookmarks, tabs and back, forward and up navigation with history.
- A thumbnail grid with folders first in natural order, adjustable item size, and folder previews that cascade
  the first pictures and videos inside each folder.
- Search by name, any attribute, or one attribute such as size, modified time or type, optionally through every
  folder below the current one.
- A path bar with clickable breadcrumbs and manual location entry.

**View**

- A picture viewer with fit modes, zoom, pan, rotate and mirror, a thumbnail bar and preloading of neighbouring
  pictures.
- Video playback with libmpv: crop, aspect ratio, a blurred backdrop, a seek display, per-file positions and
  timed video settings, with optional SVP frame interpolation built from open-svpflow.
- A slideshow, in order or shuffled, with a countdown you can place and style, and a configurable overlay.
- Viewing history: how often and for how long each item has been viewed.

**Organise**

- A playlist from any folder, kept between runs, with shuffle and saved orders. It loads and saves M3U, M3U8,
  PLS and XSPF, and it can be detached into its own window.
- Scripts: shell commands run on the item under the cursor or the one being viewed.
- Over 150 actions with keybindings in seven contexts, shown by <kbd>F1</kbd> and editable in the preferences.

**Extend**

- Python plugins add actions, keybindings, menus, events, sidebar panels and settings. They are off until each
  is enabled. Bundled plugins describe pictures with a vision model, upscale them, chat about them, and serve
  rsimgv over the Model Context Protocol.
- A remote control interface over a Unix socket, TCP or HTTP, with an Open WebUI tool in
  [`clients/open-webui`](clients/open-webui).

See the [documentation](https://topher-au.github.io/rsimgv/docs/index.html) for each feature in detail.

## Installation

### Prebuilt packages

AppImage and Flatpak builds are attached to each version on the
[releases page](https://github.com/topher-au/rsimgv/releases). They are built without Python plugin support, so the
plugins, including the MCP server, are only available in a build from source or the Arch package.

### Arch Linux

```sh
scripts/dist.sh
cd packaging/arch
makepkg -si
```

### From source

Build-time requirements:

- Rust (stable) with `cargo`
- A C compiler and `pkg-config`
- Meson 1.2 or newer, and Ninja
- GTK 4.22 or newer (`gtk4`) and libadwaita 1.9 or newer (`libadwaita-1`)
- libmpv (`mpv`) and OpenCV 4.5 or newer (`opencv`), with Clang (`clang`) for the OpenCV bindings
- `sassc`, to compile the stylesheets
- `glib-compile-resources` and `glib-compile-schemas` (part of GLib)
- Optional: `python3-embed` and `pygobject-3.0` for plugins. Configure with `-Dpython=auto` (the default),
  `-Dpython=enabled` or `-Dpython=disabled`.

```sh
git clone --recurse-submodules https://github.com/topher-au/rsimgv.git
cd rsimgv
meson setup builddir --prefix=/usr/local
meson compile -C builddir
meson install -C builddir
```

Installing copies the GSettings schema, the application icons, the desktop file and the bundled plugins into
the system data directories. The program embeds its interface, stylesheets and icons, so it needs nothing else
installed beside the libraries it links.

## Usage

```sh
rsimgv [FILE_OR_FOLDER...]
```

Each argument opens in a new tab of the open window, or of a new window if none is open. A folder opens in its
tab, and a file opens in a tab on its folder with the file selected. A picture or video also opens in the viewer.
A running rsimgv takes the arguments of a second launch. `rsimgv --version` prints the version, and
`rsimgv --verbose` prints debug output.

Press <kbd>F1</kbd> for the keys in force where you are, <kbd>Ctrl</kbd>+<kbd>,</kbd> for the preferences,
and <kbd>I</kbd> to add the item under the cursor to the [playlist](https://topher-au.github.io/rsimgv/docs/playlist.html).

## Configuration

Settings are YAML in `~/.config/rsimgv/rsimgv.conf` (`RSIMGV_CONFIG` selects another file) and are applied the
moment they change, whether from the preferences dialog, the menu or the file. Metadata and the viewing history are
kept in `media-index.sqlite3` in the user's data directory (`RSIMGV_DATA` overrides its folder). A file that cannot
be parsed makes the settings read-only until it is fixed, so it is never overwritten.

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

The [settings reference](https://topher-au.github.io/rsimgv/docs/settings.html) lists every setting and its default.

## Plugins and automation

Plugins are Python packages found in `~/.local/share/rsimgv/plugins` or any `$XDG_DATA_DIRS/rsimgv/plugins`. None
runs until it is switched on in **Preferences → Plugins**, where each plugin's settings also live. Reloading
plugins is opt-in, under the **Advanced** tab. See [docs/PLUGINS.md](docs/PLUGINS.md) for the manifest, the Python
API and the IPC behaviour.

| Bundled plugin | What it does |
| --- | --- |
| **LLM Screenshot** | Sends the viewer's picture or video frame to an OpenAI-compatible vision model and shows the description. |
| **AI Upscale** | Keeps upscaled copies of pictures, shown in the viewer, with a side-by-side comparison. |
| **LLM Chat** | A chat about the current picture in the sidebar and viewer, with memories, viewer control and voice. |
| **MCP Server** | Serves the Model Context Protocol so clients such as Claude Code or Open WebUI can drive rsimgv. |

The remote control interface is off by default. Enable a Unix socket, TCP or HTTP transport under
**General → Remote control**; the [remote control reference](https://topher-au.github.io/rsimgv/docs/remote-control.html)
lists every method.

## Development

`dev.sh` sets up the build directory if needed, rebuilds, and runs the app from it:

```sh
./dev.sh              # rebuild and run rsimgv
./dev.sh test         # rebuild and run the test suite
./dev.sh cargo check  # run a Cargo command in the build environment
```

Tests need a display. On a machine without one, run them under `xvfb-run -a`.

Pushing a `v*` tag builds the source archive, an AppImage and a Flatpak in separate GitHub workflows and attaches
each to the release. The website in [`site/`](site) is a static site built by `python3 site/build.py`.

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
  settings/          GSettings settings and YAML storage
  storage/           atomic file writes, bookmarks, per-file settings, the metadata and viewing-history database
  ui/                widgets: window, folder tree, browse view, path bar, navigation bar, preferences, split view
native/              GObject subclass for GSettings (backend.c)
data/
  ui/                GTK XML templates for every window and widget
  style/             SCSS, compiled to CSS at build time
  icons/             application and symbolic icons (hicolor theme layout)
  plugins/           the bundled plugins
  *.gschema.xml      GSettings schema
  rsimgv.gresource.xml  bundle manifest
build-aux/           build helpers that run cargo from Meson
clients/open-webui/  an Open WebUI tool that speaks the remote control interface
packaging/           Arch, AppImage and Flatpak recipes
site/                the website and documentation
```

The GObject subclass that connects GSettings to the YAML file is written in C (`native/backend.c`), because the
Rust bindings do not expose the trait needed to subclass `GSettingsBackend`. The application ID is
`au.topher.rsimgv`.

## License

rsimgv is released under the [GPL-3.0-or-later](LICENSE).
