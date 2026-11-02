1. High: folder previews read the folder on the main thread every time a folder item is shown (src/ui/browse_view.rs:307-340, called from 453-456, 1041, 1775)

Scenario: scroll past folders, resize the window so the column count changes, any folder-content refresh, or arrow-key over folders while folder cycling is on.
Evidence: scattered_media calls std::fs::read_dir on the GTK main thread and does a stat and a content-type guess for up to 500 entries. Three things call it:
style_item → request_scattered, on every bind of a folder item;
start_cycle, on every selection change via update_cycle (line 721);
the metadata callback in aspect_of (finding 9), indirectly.

Twenty visible folders means up to 10,000 stat calls per rebind. On a network share or a spinning disk the UI freezes.

take(SCATTER_SCAN) runs before sort(), so in folders with more than 500 entries the subset depends on readdir order.

Fix: do the scan with gio::spawn_blocking, cache the result per (folder, mtime) in the view, and apply it only if the item still shows that folder.

2. High: thumbnail PNGs are decoded on the main thread (src/fs/thumbnail.rs:267-272, 365-368)

Evidence: run awaits spawn_blocking and then calls gdk::Texture::from_bytes for each PNG on the main context. A hover-frames job decodes up to 10 at once, and thumbnails go up to 2048 px (16 MB each).

A disk-cache hit is worse: cached_or_made decodes the whole PNG with Pixbuf::from_file just to read tEXt::Thumb::MTime, reads the file again, and then the main thread decodes it a second time.

Fix: build the texture, or the raw pixels for a gdk::MemoryTexture, on the worker thread. Check the MTime with a header-only read, or record the mtime in the cache file name.

3. Medium: any change to the folder’s items rebinds every visible item, and refreshes apply removals one at a time (src/ui/item_grid.rs:366-375, 397-401; src/ui/browse_view.rs:1288-1300)

Evidence: the items-changed handler calls reset(), which clears all shown widgets and every measured row height, and then prune_marks(), which builds a set of every path in the folder (O(n)) whenever anything is marked.

reconcile calls store.remove(position) once per removed or changed file. A single modified file therefore rebinds the whole screen: thumbnail requests, metadata requests, and finding 1’s folder scans. k changed files cost k resets plus k O(n) prunes.

A changed file is removed and re-added as a new entry, so its mark is pruned and lost.

Fix: apply the changes to the store in one splice. In the grid, adjust shown for the actual position, removed and added counts instead of resetting, and keep marks across a remove-then-re-add.

4. Medium: a later metadata request is silently dropped while an earlier one is pending (src/fs/metadata.rs:151-161, 184-198, 206-209)

Evidence: when the key is already queued, request registers no waiter. Only the first requester’s wanted/on_ready survive. If the first requester stops wanting the data, the job is either dropped in next_wanted or finished without calling anyone.
Scenario: a folder’s preview requests metadata for a picture, then the user opens that folder. The item for that picture is swallowed as a duplicate, the folder item stops showing the folder, and the job is dropped, so the info line never fills in. Scrolling away and back has the same effect, because the returning position is usually bound to a different widget.
Fix: keep every waiter, deduplicating by cell if needed, and treat the job as wanted if any waiter wants it.

5. Medium: thumbnail results can be stale because “still wanted” only checks the path (src/ui/browse_view.rs:256-262, 283-298, 471-487, 1960-1965)

Evidence: wanted_for checks only that the cell’s position still maps to the same path. It ignores item size, crop, preview count or fit mode, and hover state.

After refresh_items, spare cells are handed out in the arbitrary order the HashMap was drained in. When an old job’s cell happens to be rebound to the same item, the old job is still “wanted”. It often finishes after the new size’s job (the queue is newest first) and overwrites the item with the old set_pixel_size, old crop or old ScatteredThumbnail. This shows up as some thumbnails randomly appearing at the old size after a resize.

Hover frames use the same check, so moving the pointer across many videos leaves every frames job (one ffprobe plus up to 10 ffmpeg runs) “wanted”. Those jobs hold the 4 job slots while new still thumbnails wait.

Fix: store a bind counter per cell (for example with set_data or a CSS-independent Cell) and capture it in wanted. For frames, also require is_current_hover(generation).

6. Medium: failed thumbnails are retried on every bind (src/fs/thumbnail.rs:209-221)

Evidence: when textures is None, nothing is remembered in memory or on disk. Corrupt files, formats with no loader, and videos ffmpeg cannot read are decoded again, or re-run through ffprobe/ffmpeg, on every scroll-in and every reset from finding 3.
Fix: keep a failure set keyed like Key (path and mtime), and optionally write a fail marker on disk as the freedesktop thumbnail spec does.

7. Medium: a folder change during a refresh can be lost (src/ui/browse_view.rs:1246-1250)

Evidence: refresh returns immediately while reading is set and records nothing. If the folder changes after the enumeration has passed that entry, the refresh timer fires, sees a read in progress and gives up. The change stays invisible until the next monitor event.
Fix: set a dirty flag when a refresh is skipped, and call refresh again in on_done if it is set.

8. Medium: sorting and filtering allocate several strings per comparison on the main thread (src/ui/browse_view.rs:684-705, 670-681, 505-512; src/fs/entry.rs:47-49, 65-67)

Evidence: every comparison clones show_directories(), sort_by() and sort_order(), all RefCell<String> properties. It also allocates two name() strings, and sorting by type adds two content_type() strings. That is about 5 to 7 heap allocations per comparison, plus to_lowercase work in natural_cmp, for every batch of a large folder. The filter also allocates name() and show_directories() for every item.
Fix: cache the sort and filter settings as enums in Cells, and cache a precomputed name or sort key in FileEntry.

9. Medium: metadata arriving for a fitted folder rebuilds that folder’s item (src/ui/browse_view.rs:1857-1865)

Evidence: the aspect_of callback calls style_item on the folder’s item. That re-runs the folder scan from finding 1, builds a new ScatteredThumbnail and re-requests every preview, and it can happen up to 12 times per page. Because still_cycling compares the ScatteredThumbnail object, a running cycle stops at its next tick and does not restart until the pointer re-enters.
Fix: on metadata arrival, recompute only the page length and update the existing ScatteredThumbnail.

10. Low: a leftover “after loading” action runs on the wrong folder (src/ui/browse_view.rs:1614-1622, 1166-1173; src/ui/window.rs:578-584)

Evidence: neither stop_loading nor set_path clears pending, and show_path sets it only when there is a name to select. If you stop or leave folder A’s load and then open folder B, A’s select_named runs on B. It also sets selecting = true, which skips restoring B’s saved scroll position.
Fix: clear pending in stop_loading and at the start of set_path. The caller then sets it again after set_path if it needs one.

11. Low: once anything is marked, every selection change costs O(n) (src/ui/browse_view.rs:1643; src/ui/item_grid.rs:415-421)

Evidence: update_status calls marked_paths(), which walks the whole model and allocates a PathBuf per item. After select-all in a 50k-item folder, every arrow key does this.
Fix: use marked.len(), since the marks are already pruned when items change.

12. Low: items nobody can see can keep thumbnail jobs alive, and the queue grows (src/ui/item_grid.rs:698-704, 785-793; src/fs/thumbnail.rs:163-171)

Evidence: spare (hidden) cells keep their old widget_name, so shows() still reports them as showing their old item, and their jobs stay wanted.

Jobs nobody wants are only removed when they reach the end of the queue. Fast scrolling through a huge folder therefore builds a long queue, and every repeat request does a linear queue.iter().position scan.

Fix: clear the widget name when a cell goes to spare. Use an indexed set or generation counter so stale queue entries can be removed or re-prioritised without a linear scan.

13. Low: concurrent cache writes can collide (src/fs/thumbnail.rs:392)

Evidence: the temporary file name depends only on the process id. Two jobs for the same output file, such as the same path with a different mtime after the file is modified, write the same temporary file at the same time.
Fix: add a per-call counter or random suffix to the temporary name.

14. Low: thumbnails are blurry on HiDPI screens (src/ui/browse_view.rs:287, src/fs/thumbnail.rs:38)

Evidence: Size::for_pixels(size) ignores scale_factor(), so at scale 2 a 128 px thumbnail is stretched to 256 device pixels.
Fix: request a size of size * widget.scale_factor().
Suspicions (plausible, not proven)
Loading may finish before the already-read entries are delivered (src/fs/store.rs:139-154, 208-226). A view that joins a read already in progress, for example one the folder tree started, gets the entries read so far through a separate main-loop task. If the read finishes before that task runs, on_done fires first: the selection is attached, pending runs on a partial model, and the entries arrive afterwards. Whether this happens depends on the order the main loop dispatches them, which I did not confirm.
An empty “selection-changed” fires when the selected file is modified (src/ui/browse_view.rs:1297 and 711-720). In reconcile, removing the selected entry emits selection-changed("") before it is selected again by name, so listeners such as the viewer briefly see “no selection”. I confirmed the emission but not its effect on the listeners.
Sorting cost while loading. The SortListModel re-sorts on each 100-entry batch even while the grid is detached. The cost depends on GTK internals, but combined with finding 8 it is likely to dominate loading time for very large folders.
Cancelling a folder read doesn’t stop the read already in progress. The cancellable is never passed to the enumerate/next-files calls (src/fs/store.rs:258-262), so a cancelled read stops only after the current batch’s I/O completes.
