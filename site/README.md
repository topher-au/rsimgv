# rsimgv website

The website and documentation for [rsimgv](https://git.topher.au/topher/rsimgv), an integrated file explorer and
media viewer for GNOME.

It is a static site: HTML fragments in `content/`, wrapped in a shared layout by `build.py` and written to `public/`.
There are no dependencies beyond Python 3.

## Build and preview

```sh
python3 build.py            # writes public/
python3 build.py --serve    # builds, then serves http://localhost:8000
```

`public/` is plain files; publish it with any static host.

## Layout

| Path | What |
| --- | --- |
| `content/*.html` | Top-level pages: home, features, download |
| `content/docs/*.html` | The documentation. The sidebar order is the `DOCS` list in `build.py` |
| `content/_*.html` | Generated fragments (see below); not pages |
| `static/` | `style.css` (light and dark themes) and `site.js` (theme, lightbox, docs search) |
| `assets/` | Icon and `screenshots/*.webp` |
| `tools/extract_reference.py` | Generates the keybinding and settings references from rsimgv's source |
| `tools/screenshots/` | How the screenshots were made, with scripts for the demo media and a sandboxed launcher |

A page starts with `key: value` lines (`title`, `desc`, optionally `layout` and `wide`), then a line holding `---`,
then HTML. Shortcodes: `{{include _file.html}}`, `{{shot name "Caption"}}` (a lightbox figure for
`assets/screenshots/name.webp`) and `{{root}}`. Docs pages get a sidebar, an on-page contents list built from their
`h2` and `h3` headings, previous and next links, and an entry in the search index.

## Keeping the references in step with rsimgv

The action table in *Keybindings and mouse* and the *Settings reference* are generated, so they match the code.
After the app's actions or settings change, regenerate them from a checkout of rsimgv and rebuild:

```sh
python3 tools/extract_reference.py ../rsimgv
python3 build.py
```

It reads `src/keybindings/actions.rs`, `src/settings/store.rs` and `data/au.topher.rimgv.gschema.xml`, and warns about
any setting it has no section for (add it to `SECTIONS` in the script).

The rest of the documentation is written by hand from the source and the behaviour of the app, so check it against
the app when features change.

## Screenshots

Taken from a real build running on synthetic demo pictures and clips, in a sandbox, so no personal files appear.
See `tools/screenshots/README.md` to retake them.

## Licence

This site's source is released under the GNU General Public Licence, version 3 or later, the same as rsimgv. See `LICENSE`.
