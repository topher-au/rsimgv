# rsimgv review: file system access and thumbnails

Two read-only reviews of the working tree on branch `experiment`, including uncommitted changes. Neither review edited files or ran builds or tests. Line numbers refer to the tree as it was reviewed and may shift once uncommitted changes are committed.

- Part 1: thumbnail generation and caching
- Part 2: file system access

---

## Part 1: Thumbnail pipeline

### How it works

`Thumbnails` (`src/fs/thumbnail.rs`) is one cache per thread, all on the main thread (`Rc`/`RefCell`). It keeps a newest-first (LIFO) queue with at most `MAX_JOBS = 4` jobs running at once. Each job runs `make()` through `gio::spawn_blocking`.

The disk cache is `~/.cache/rsimgv/<size>/<md5(uri)>.png`, plus `frames/<size>/<md5>-<i>.png` for video hover frames, with an `MTime` text tag in each PNG. The in-memory cache is a 256 MB FIFO. A request is dropped when its `wanted()` closure says the cell no longer shows that path.

### Findings

**1. High: cached PNGs are decoded three times, once on the main thread.**
- `thumbnail.rs:365` decodes the whole cached PNG (`Pixbuf::from_file`) just to read the MTime tag.
- `:367` then reads the file again from disk.
- `:270` decodes it a third time with `gdk::Texture::from_bytes`, inside `run()`, which runs on the main loop (`spawn_future_local`).
- Scenario: scrolling a folder whose thumbnails are already on disk. Every thumbnail costs a PNG decode on the UI thread, up to 16 MB each at 2048 px, so scrolling stutters.
- Fix: read the bytes once, check the tag by parsing the PNG chunks without a full decode, and build the texture on the worker (`Texture::from_bytes` or `MemoryTexture` there, then send it back).

**2. High: folder previews scan directories synchronously on the main thread.**
- `scattered_media` (`browse_view.rs:307-334`) runs `read_dir`, `metadata()` and `content_type_guess` on up to 500 entries. It is called from `request_scattered` (`:456`) on every bind (`:1041`), and from `start_cycle` (`:1775`) on hover or selection.
- Each metadata reply in `aspect_of` calls `style_item` (`:1862`), which rebinds the whole cell and rescans the folder. That is up to 12 times per folder in fitted mode.
- Scenario: a parent folder of photo folders on a slow disk or NFS. Scrolling freezes.
- Fix: scan in a worker and cache the result per folder (path + mtime). On metadata arrival, recompute only the page layout instead of calling `style_item`.

**3. Medium: any model change rebinds every visible cell.**
- The `items_changed` handler (`item_grid.rs:366-373`) calls `reset()` (all cells, row heights, table) plus `prune_marks()`, which is O(n).
- `reconcile` removes items one at a time (`browse_view.rs:1297`), so N removals mean N resets and N prune passes.
- Scenario: copying files into the open folder triggers a refresh on `ChangesDoneHint`. Every visible cell rebinds: icons flash back where thumbnails were evicted, and every folder cell is rescanned (see 2).
- Fix: use one `splice` per contiguous range, and in the handler only reset positions at or after the change.

**4. Medium: failed thumbnails are retried forever.**
- `finish(key, None)` (`thumbnail.rs:209-220`) drops the waiters and stores nothing.
- Scenario: a corrupt JPEG or a video ffmpeg cannot read. Every rebind and every scroll back re-runs the decode or ffmpeg and ties up a job slot each time.
- Fix: keep a negative-cache entry keyed by (path, mtime), in memory and optionally as a marker file on disk.

**5. Medium: ffmpeg/ffprobe have no timeout and running jobs are never cancelled.**
- See `thumbnail.rs:319-334` and `:344-348`.
- Scenario: four videos on a stalled network share hang ffmpeg, and all thumbnailing stops.
- Fix: spawn the process, wait with a timeout and kill it; also check `wanted` between frames.

**6. Medium: hover frame jobs block still thumbnails.**
- The frames request uses `wanted_for(cell, path)` (`browse_view.rs:1964`). That stays true after the pointer leaves, because the cell still shows the file.
- Each frames job runs ffprobe plus up to 10 ffmpeg calls in series (`thumbnail.rs:288-297`), sharing the same 4 slots.
- Scenario: moving the mouse across a grid of videos queues many frame jobs, and stills for newly visible items wait behind them.
- Fix: make the frames request "wanted" only while that hover generation is current, and give stills priority or a separate pool.

**7. Medium: stale results can overwrite a newer bind of the same path.**
- `wanted` only checks the path (`browse_view.rs:256-262`). The callback captured `size`, `crop` and, for folders, the `scattered` object at request time (`:282-297`, `:477-485`).
- Scenario: resizing items 128 to 300 changes the size class. The new 512 px request is answered; the old 128 px job finishes later and sets `set_pixel_size(128)` with a low-resolution texture, or puts the old `ScatteredThumbnail` back.
- Fix: stamp each cell with a bind generation, or include the size class in the check, and verify it in `on_ready`.

**8. Medium: the disk cache grows without limit.**
- There is no eviction anywhere in `thumbnail.rs`. There are 7 size directories, plus up to 10 frame files per video per size.
- Renamed or deleted files leave orphan entries (the key is the URI). `.tmp` files left by failed writes (`:392-394`) are never cleaned up.
- Fix: add an LRU or age-based sweep (by atime/mtime) at startup or when idle, and remove `*.tmp` files.

**9. Medium: frames are all-or-nothing, and ffprobe runs even on a cache hit.**
- `video_duration` (`thumbnail.rs:288`) spawns ffprobe before looking at the cached frames.
- `collect::<Option<Vec>>` (`:291-297`) throws the whole set away if one frame fails, then retries it all on the next hover (see 4).
- Fix: store the frame count or duration alongside the cache, and keep the frames that succeeded.

**10. Low-medium: temporary file names can collide within the process.** Unverified in practice.
- The temp name is only `<file>.png.<pid>.tmp` (`thumbnail.rs:392`).
- Two keys can map to the same cache file. The key includes `media` and `mtime`, but the file name does not.
- Scenario: the thumbnail bar classifies media by extension (`is_video`) while the browse view uses content type. If they disagree, two workers write the same temp file at once and the rename can publish an interleaved PNG. The same happens if a file's mtime changes while a job runs.
- Fix: add a unique counter or thread id to the temp name, and pick the media type one consistent way.

**11. Low-medium: non-UTF-8 paths break in the viewer's thumbnail bar.**
- `thumbnail_bar.rs:339` stores `to_string_lossy()` in a `StringObject`, and `:440` turns it back into a `PathBuf`.
- Names with invalid UTF-8 get a path that does not exist, so decoding fails, and that is retried forever (see 4).
- Fix: store `gio::File` or `FileEntry` objects in the model instead of strings.

**12. Low: the queue costs O(n) per request and is pruned lazily.**
- `request` scans the queue to re-prioritise (`thumbnail.rs:167-171`), and the waiter lists keep growing on rebind.
- After fast scrolling through thousands of items, every bind pays a linear scan. Unwanted keys are only dropped when popped.
- Fix: index the queue by key (e.g. an `IndexMap` or a generation counter), or prune unwanted entries when the visible range changes.

**13. Low: the cache key uses whole-second mtime.**
- See `browse_view.rs:241` and `thumbnail.rs:366`. A file rewritten within the same second keeps its stale thumbnail, both in memory and on disk, even though `reconcile` re-adds the item.
- Fix: include nanoseconds and the file size in the key and the tag.

**14. Low: the thumbnail size ignores the display scale factor.**
- `Size::for_pixels(size)` (`browse_view.rs:287`, `thumbnail_bar.rs:464`) uses logical pixels, so thumbnails are upscaled and blurry on 2x displays.
- Fix: multiply by `scale_factor()`.

**15. Low: other small issues.**
- The in-memory cache is FIFO, not LRU: hits don't refresh order (`thumbnail.rs:223-237`). Textures held by widgets also sit outside the 256 MB budget.
- `thumbnail_bar` calls `stat` on the main thread for every bind (`:33`, `:462`).
- `request` keeps the `textures` RefCell borrowed while calling `on_ready` under edition 2021 (`:154-156`). It is safe today, but it will panic if a callback ever triggers `remember` synchronously. Unverified as a live bug.

### Top findings, ranked

1. Triple PNG decode, one of them on the main thread (`thumbnail.rs:270`, `:365-367`)
2. Synchronous folder scans on bind, repeated per metadata reply (`browse_view.rs:307`, `:456`, `:1862`)
3. `items_changed` resets every cell, and `reconcile` removes items one at a time (`item_grid.rs:366`, `browse_view.rs:1297`)
4. Failed decodes retried forever (`thumbnail.rs:209-220`)
5. No ffmpeg timeout or cancellation (`thumbnail.rs:319`, `:344`)
6. Hover frame jobs hold slots after the pointer leaves (`browse_view.rs:1964`)
7. Stale size, crop or scatter results applied after a rebind (`browse_view.rs:282-297`, `:477-485`)
8. Disk cache grows without limit, with orphaned and tmp files (`thumbnail.rs:283`, `:293`, `:392`)

### Files read

- `src/fs/thumbnail.rs`
- `src/ui/thumbnail_paint.rs`
- `src/ui/item_grid.rs`
- `src/ui/browse_view.rs` (lines 220-520, 690-905, 960-1320, 1760-2045)
- `src/ui/thumbnail_bar.rs` (lines 30-40, 270-489)
- `src/ui/media_viewer.rs` (lines 1085-1135)
- `src/fs/metadata.rs` and `src/ui/window.rs` (searched only)

---

## Part 2: File system access

### Findings

**1. High: synchronous folder scan for every folder cell, on the main thread.**
- `src/ui/browse_view.rs:307-340` (`scattered_media`) is called from `request_scattered` (`:456`). That runs from `style_item` (`:1041`) every time a cell is bound. It does `read_dir`, takes up to 500 entries, and for each one runs `path.metadata()` (a stat) plus `content_type_guess`, with no caching. It is called again on hover in `start_cycle` (`:1775`). In fitted mode, each metadata result calls `style_item` again (`:1862`), so the same folder is rescanned once per picture.
- Scenario: a grid of 50 subfolders on NFS or USB means about 25,000 stats on each scroll page, and the grid stutters or freezes.
- Fix: cache the result per folder (keyed by the folder's mtime) and build it with `spawn_blocking` or the `FileStore`. In the metadata callback, update only the previews instead of calling `style_item` again.

**2. High: folder stepping walks the disk on the UI thread with no limit, and follows symlinks.**
- `src/ui/window.rs:1126-1132` and `1166-1171` call `src/fs/walk.rs:10-20` (`child_directories`) and `:63-66` (`has_media`) synchronously. `entry.path().is_dir()` and `is_file()` stat every entry and follow symlinks. `next_folder` and `previous_folder` re-read the parent's sibling list at each level, and `next_media_folder` loops until it finds media.
- Scenario: stepping to the next media folder from a deep tree with no media below it scans the whole remaining tree while the UI is frozen. A directory with two symlinks that point back up the tree gives an almost unbounded walk, which only stops when paths hit the ELOOP or name-length limits.
- Fix: run the walk on a worker with a `Cancellable`, use `DirEntry::file_type()` (which does not follow symlinks), and skip symlinked directories or track visited inodes.

**3. High: the viewer loads each item with blocking I/O and a full decode on the main thread.**
In `src/ui/media_viewer.rs`, `load()` does all of this synchronously on every step:
- `refresh_status` stats the file (`:1092`).
- `FileSettings::for_path` (`:1145`) opens the file, reads two 64 KiB chunks for the hash (`src/file_settings.rs:28-47`) and reads a YAML file (`:70`).
- `gdk::Texture::from_file` (`:86` and `:1206`) decodes the full-resolution image.
- `release()` writes the settings file (`:1228`, `file_settings.rs:130-144`).

Nothing preloads the neighbouring items.
- Scenario: holding the arrow key through 40 MP JPEGs on a slow disk. Each step blocks for hundreds of milliseconds and keypresses queue up.
- Fix: decode asynchronously (gdk-pixbuf async or `spawn_blocking`) and ignore a result if the index has changed since. Prefetch the items on either side, and hash and save settings off the UI thread.

**4. Medium-high: a settings reload can wipe the user's settings.**
- In `src/settings/store.rs`, the monitor at `:355-364` reloads on `Changed` and `Deleted`. `load` turns an unreadable or unparsable file into an empty map (`:176-178`, `unwrap_or(Value::Null)`). `reload` (`:325-335`) then replaces `store.values` with that empty map, and the next `rsimgv_store_write` (`:419`) saves only what is in memory.
- Scenario: an editor that truncates then writes fires `Changed` while the file is half-written, or a typo makes the YAML invalid. Every setting resets to its default in the UI, and the next app write replaces the file with that reduced set.
- Fix: react only to `ChangesDoneHint` (and moves). On a parse error, keep the current values instead of replacing them.

**5. Medium: a second metadata request for a file that is already pending gets no answer.**
- `src/fs/metadata.rs:151-161` stores no waiter for the second request. `next_wanted` (`:184-197`) and `finish` (`:206-209`) only check the first waiter, so if the first requester no longer wants the result, the job is dropped or its result is never delivered.
- Scenario: a grid cell requests metadata for X and is then recycled. Later the viewer (`media_viewer.rs:1105`) or another cell asks for X. Its resolution or info line never appears until the item is bound again.
- Fix: keep every waiter, or replace the stored waiter's `wanted` and `on_ready` with the newest ones.

**6. Medium: PNG thumbnails are decoded on the main thread, and a cache hit reads the file three times.**
- `src/fs/thumbnail.rs:267-272` calls `gdk::Texture::from_bytes` in the main-loop future. On a cache hit, the worker fully decodes the cached PNG just to read one tEXt field (`:365`), then reads the file again (`:367`).
- Scenario: fast scrolling at large item sizes (thumbnails up to 2048 px, 16 MB each) causes frame drops.
- Fix: build the texture in the `spawn_blocking` closure, and check the mtime with a PNG chunk reader or a sidecar file instead of a full decode.

**7. Medium: ffmpeg and ffprobe have no timeout or cancellation, and the frames job ties up a slot.**
- `thumbnail.rs:319-334` and `:344-348` call `Command::output()` with no timeout. A frames job runs ffprobe plus up to 10 ffmpeg runs one after another in a single slot (`:288-297`). There are only 4 slots (`:24`), and a job cannot be cancelled once it has started.
- Scenario: hovering several videos, or one hung network or corrupt file, fills all slots and thumbnails stop arriving.
- Fix: add a timeout or kill to each subprocess, check a cancellation flag between frames, and keep a separate low-priority lane for frames.

**8. Medium: failures are not remembered.**
- `thumbnail.rs:208-221` discards the waiters when a job produces nothing and records nothing.
- Scenario: corrupt files, or videos when ffmpeg is not installed, are re-decoded and the processes re-spawned on every bind.
- The same path also stalls folder-preview cycling. `browse_view.rs:1885-1895` only decrements `remaining` when a texture is ready, and `advance_cycle` waits for it to reach 0 (`:1816`), so one bad picture freezes the cycle while its 1 s timer keeps firing.
- Fix: cache failures per key (path and mtime) and call back with "no texture" so the preload can count it.

**9. Medium: the folder tree never refreshes.**
- `src/fs/node.rs:150-153`: once a node is `Loaded` it stays loaded, and the tree has no monitor (`fs_tree.rs:284` only calls `start_loading`). Folders created or deleted later never appear or disappear in the tree, although the browse view, which has its own monitor at `browse_view.rs:1206`, updates.
- Fix: monitor expanded nodes, or reload when a folder is revealed.

**10. Medium (unverified): late joiners to a listing can get callbacks out of order.**
- `src/fs/store.rs:139-154`: a reader that joins a read in progress gets the earlier entries through a separately spawned replay future. Meanwhile `append` and `finish` (`:208`, `:224`) may reach it first. In `node.rs:181-196`, `on_done` takes `found` and splices it in, so entries replayed after that are lost.
- Scenario: the browse view and the tree read the same folder at the same moment during navigation.
- Fix: deliver the snapshot synchronously in `read()`, or queue per-reader events in order.

**11. Low-medium: bookmarks and keybindings are written in place.**
- `src/bookmarks.rs:90` and `src/keybindings/mod.rs:45` use a plain `std::fs::write`. A crash can truncate the file. `load` then leaves an empty list, and the next save makes the loss permanent.
- Fix: write a temp file and rename it, as `file_settings.rs:141-143` already does.

**12. Low-medium: two windows can overwrite each other's per-file settings.**
- `file_settings.rs` keeps a snapshot per open file. Two windows viewing the same file each save their whole map (`:140-143`), so the last one to save wins.
- Fix: re-read and merge before saving.

**13. Low: same-name temp files in the thumbnail cache.**
- `thumbnail.rs:392` names the temp file only by the process ID. Two jobs writing the same `digest.png` at once write to the same temp file. This happens when the key differs only in mtime, or in `Media`, because the thumbnail bar guesses the type from the name (`thumbnail_bar.rs:448`) while the grid uses the GIO content type. The on-disk cache is also never pruned.

**14. Low: smaller main-thread stats and one non-UTF-8 gap.**
- The thumbnail bar stats each row on bind (`thumbnail_bar.rs:33-38`, `:462`).
- `show_properties` calls `query_info` synchronously (`window.rs:546`).
- `scattered_media` skips names that are not UTF-8 (`browse_view.rs:314`).
- `reconcile` keys entries by display name (`browse_view.rs:1285`), which can collide for names that are not UTF-8.

**15. Low: memory caches with no real limit.**
- `FileStore` keeps all entries of up to 16 idle listings with no size cap (`store.rs:25`).
- `MediaInfo.known` grows without limit (`metadata.rs:98`).

### Top findings, ranked

1. Folder-preview scan on bind, hover and metadata callbacks (`browse_view.rs:307`, `:456`, `:1775`, `:1862`)
2. Synchronous, symlink-following folder walk (`window.rs:1126`, `walk.rs:10`, `:63`)
3. Viewer: full decode, hash and settings I/O on the UI thread (`media_viewer.rs:86`, `:1145`, `:1206`, `:1228`)
4. Settings reload wipes values on a partial or invalid file (`settings/store.rs:176`, `:325`, `:355`)
5. Second metadata request gets no callback (`metadata.rs:151`)
6. Texture decode on the main thread and triple read of cached thumbnails (`thumbnail.rs:270`, `:365`)
7. No timeout or cancellation for ffmpeg, and the frames job holds a slot (`thumbnail.rs:288`, `:319`)
8. Failures not cached, and folder-preview cycling stalls (`thumbnail.rs:208`, `browse_view.rs:1816`)

### Files read

- In `src/fs`: `mod.rs`, `entry.rs`, `walk.rs`, `node.rs`, `sort.rs`, `store.rs`, `thumbnail.rs`, `metadata.rs`.
- In `src/ui` (relevant sections): `browse_view.rs`, `window.rs`, `media_viewer.rs`, `thumbnail_bar.rs`, `fs_tree.rs`, `drag.rs`, `keybinding_list.rs`, `mod.rs`.
- Also `src/file_settings.rs`, `src/settings/store.rs`, `src/bookmarks.rs`, `src/keybindings/mod.rs`, `src/main.rs` (`:195-220`), and the relevant sections of `src/remote.rs`, `src/mpv/mod.rs` and `src/ipc.rs`.
- `src/ui/item_grid.rs` and `src/ui/image_canvas.rs` were checked by grep only and do no direct disk access.
