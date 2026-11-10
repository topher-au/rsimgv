//! The app's shared store of folder listings. One `FileStore` serves the whole process (the folder
//! tree, the browse view, ...), so a folder is read once and every reader gets the same `FileEntry`
//! objects.
//!
//! Like Nautilus's directory loading, a read is streamed: readers are given each batch of entries as it
//! arrives. A reader that joins a read already in progress is first given the entries read so far. A
//! read that no reader is waiting for any more is cancelled.
//!
//! A listing someone watches is also kept up to date, as Nautilus does: a monitor on the folder reports changes to
//! files (see `monitor`), and each is applied to the listing by looking at only the file it names, then told to the
//! watchers as the entries that were added, changed, removed or moved. Changes are applied in the order they came.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use gtk::{gio, glib};

use super::changes::{self, Change};
use super::monitor::DirMonitor;
use super::FileEntry;

/// The attributes read for every entry.
const QUERY_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size,\
standard::icon,standard::content-type,standard::is-hidden,time::modified";
/// Entries requested per batch, as Nautilus does.
const BATCH_SIZE: i32 = 100;
/// Finished listings that nobody reads are kept this many at most; the least recently used go first.
const MAX_IDLE_LISTINGS: usize = 16;
/// The entries of the finished listings that nobody reads are kept this many at most, so a huge folder cannot keep
/// the memory it was listed into.
const MAX_IDLE_ENTRIES: usize = 100_000;
/// A run of additions or changes longer than this is not looked at file by file: the watchers are told to list the
/// folder again, which is cheaper than a query for each file.
const MAX_INCREMENTAL: usize = 200;

thread_local! {
    static SHARED: FileStore = FileStore::default();
}

/// A handle on the store. Clones share the same listings.
#[derive(Clone, Default)]
pub struct FileStore {
    inner: Rc<Inner>,
}

#[derive(Default)]
struct Inner {
    listings: RefCell<HashMap<PathBuf, Listing>>,
    next_reader: Cell<u64>,
    tick: Cell<u64>,
    /// Runs of changes waiting to be applied, oldest first, and whether they are being applied now. A run is applied
    /// only after the one before it, since applying looks at the disk and so takes time.
    pending: RefCell<VecDeque<Vec<Change>>>,
    applying: Cell<bool>,
}

#[derive(Clone)]
enum State {
    Loading,
    Loaded,
    Failed(String),
}

struct Listing {
    state: State,
    entries: Vec<FileEntry>,
    readers: Vec<Reader>,
    /// Stops the read that fills this listing.
    cancel: gio::Cancellable,
    /// When the listing was last read, for choosing which idle listings to keep.
    used: u64,
    /// Who is told of changes to the listing. While there is any, a monitor watches the folder.
    watchers: Vec<Watcher>,
    monitor: Option<DirMonitor>,
    /// Whether the folder changed while it was being read, so that the listing may lack the change.
    stale: bool,
}

/// What happened to a watched listing.
#[derive(Clone)]
pub enum ListingChange {
    /// Entries for files that appeared.
    Added(Vec<FileEntry>),
    /// New entries for files whose contents or attributes changed.
    Changed(Vec<FileEntry>),
    /// The paths of files that are gone.
    Removed(Vec<PathBuf>),
    /// Files that moved within the folder: each old path with the entry that replaces it.
    Moved(Vec<(PathBuf, FileEntry)>),
    /// The listing cannot be brought up to date from changes, so the folder should be listed again.
    Reload,
}

type OnChange = Rc<dyn Fn(&ListingChange)>;

#[derive(Clone)]
struct Watcher {
    id: u64,
    on_change: OnChange,
    active: Rc<Cell<bool>>,
}

/// A watcher's hold on a folder's listing. Cancelling it stops its callbacks, and the folder is no longer monitored
/// once nobody watches it.
pub struct Watching {
    store: FileStore,
    dir: PathBuf,
    id: u64,
    active: Rc<Cell<bool>>,
}

impl Watching {
    pub fn cancel(&self) {
        self.active.set(false);
        self.store.unwatch(&self.dir, self.id);
    }
}

/// Receives a batch of entries as it arrives.
type OnBatch = Rc<dyn Fn(&[FileEntry])>;
/// Receives how a read ended.
type OnDone = Rc<dyn Fn(Result<(), String>)>;

#[derive(Clone)]
struct Reader {
    id: u64,
    on_batch: OnBatch,
    on_done: OnDone,
    /// Cleared by `Reading::cancel`, so no callback reaches a reader that has let go.
    active: Rc<Cell<bool>>,
}

/// A reader's hold on a folder's listing. Cancelling it stops its callbacks.
pub struct Reading {
    store: FileStore,
    dir: PathBuf,
    id: u64,
    active: Rc<Cell<bool>>,
}

impl Reading {
    /// Stops the callbacks. If this was the last reader of a read still in progress, the read is cancelled.
    pub fn cancel(&self) {
        self.active.set(false);
        self.store.detach(&self.dir, self.id);
    }
}

impl FileStore {
    /// The store shared by the whole app.
    pub fn shared() -> Self {
        SHARED.with(Self::clone)
    }

    /// Reads `dir`. `on_batch` gets each batch of entries as it arrives; `on_done` runs once the read
    /// ends, with the error if it failed. A folder already listed is not read again: its entries are
    /// given to the reader straight away.
    pub fn read(
        &self,
        dir: &Path,
        on_batch: impl Fn(&[FileEntry]) + 'static,
        on_done: impl Fn(Result<(), String>) + 'static,
    ) -> Reading {
        let id = self.inner.next_reader.get();
        self.inner.next_reader.set(id + 1);
        let active = Rc::new(Cell::new(true));
        let reader = Reader {
            id,
            on_batch: Rc::new(on_batch),
            on_done: Rc::new(on_done),
            active: active.clone(),
        };
        let used = self.next_tick();

        let (fresh, cancel, entries, done) = {
            let mut listings = self.inner.listings.borrow_mut();
            let fresh = !listings.contains_key(dir);
            let listing = listings.entry(dir.to_path_buf()).or_insert_with(|| Listing {
                state: State::Loading,
                entries: Vec::new(),
                readers: Vec::new(),
                cancel: gio::Cancellable::new(),
                used,
                watchers: Vec::new(),
                monitor: None,
                stale: false,
            });
            listing.used = used;
            listing.readers.push(reader.clone());
            // A reader that joins a finished listing is told it is done; one that joins a read in
            // progress is told when that read ends, so it is not told twice.
            let done = match &listing.state {
                State::Loading => None,
                State::Loaded => Some(Ok(())),
                State::Failed(err) => Some(Err(err.clone())),
            };
            (fresh, listing.cancel.clone(), listing.entries.clone(), done)
        };

        if fresh {
            glib::spawn_future_local(read(self.clone(), dir.to_path_buf(), cancel));
        } else {
            // The entries listed so far are given now, before anything read after them, and the end of the read, if it
            // has come, is given on the main loop.
            if !entries.is_empty() {
                (reader.on_batch)(&entries);
            }
            if let Some(result) = done {
                glib::spawn_future_local(async move {
                    if reader.active.get() {
                        (reader.on_done)(result);
                    }
                });
            }
        }

        Reading {
            store: self.clone(),
            dir: dir.to_path_buf(),
            id,
            active,
        }
    }

    /// Forgets `dir`'s listing if it has finished and nobody is reading it, so the next read lists the
    /// folder afresh. A listing that is watched is kept: it is being kept up to date.
    pub fn discard_idle(&self, dir: &Path) {
        let mut listings = self.inner.listings.borrow_mut();
        let idle = listings.get(dir).is_some_and(|listing| {
            listing.readers.is_empty() && listing.watchers.is_empty() && !matches!(listing.state, State::Loading)
        });
        if idle {
            listings.remove(dir);
        }
    }

    /// Tells `on_change` of changes to `dir`'s listing, and monitors the folder for as long as anyone watches it. The
    /// folder has to have a listing already, from a read; there is nothing to watch otherwise.
    pub fn watch(&self, dir: &Path, on_change: impl Fn(&ListingChange) + 'static) -> Option<Watching> {
        let id = self.inner.next_reader.get();
        self.inner.next_reader.set(id + 1);
        let active = Rc::new(Cell::new(true));
        let mut listings = self.inner.listings.borrow_mut();
        let listing = listings.get_mut(dir)?;
        listing.watchers.push(Watcher { id, on_change: Rc::new(on_change), active: active.clone() });
        if listing.monitor.is_none() {
            listing.monitor = Some(DirMonitor::new(dir));
        }
        Some(Watching { store: self.clone(), dir: dir.to_path_buf(), id, active })
    }

    /// Drops the watcher `id` of `dir`'s listing, and the monitor if that was the last.
    fn unwatch(&self, dir: &Path, id: u64) {
        let monitor = {
            let mut listings = self.inner.listings.borrow_mut();
            let Some(listing) = listings.get_mut(dir) else { return };
            listing.watchers.retain(|watcher| watcher.id != id);
            if listing.watchers.is_empty() {
                listing.monitor.take()
            } else {
                None
            }
        };
        drop(monitor);
        self.trim();
    }

    /// Applies changes to the listings they touch. The changes are applied one run of a kind at a time, each run only
    /// once the one before it is done.
    pub fn notify(&self, changes: Vec<Change>) {
        self.inner.pending.borrow_mut().extend(changes::batches(changes));
        if self.inner.applying.replace(true) {
            return;
        }
        let store = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let next = store.inner.pending.borrow_mut().pop_front();
                let Some(run) = next else { break };
                store.apply(run).await;
            }
            store.inner.applying.set(false);
        });
    }

    async fn apply(&self, run: Vec<Change>) {
        match run[0] {
            Change::Added(_) | Change::Changed(_) => self.apply_updates(run).await,
            Change::Moved { .. } => self.apply_moves(run).await,
            Change::Removed(_) | Change::Unmounted(_) => self.apply_removals(run),
        }
    }

    /// Looks at each file of a run of additions or changes, and puts what it finds in the listing of its folder. A
    /// change to a file the listing does not have is ignored: if the file is new, an addition comes for it.
    async fn apply_updates(&self, run: Vec<Change>) {
        let adding = matches!(run[0], Change::Added(_));
        let paths: Vec<PathBuf> = run
            .into_iter()
            .filter_map(|change| match change {
                Change::Added(path) | Change::Changed(path) => Some(path),
                _ => None,
            })
            .collect();
        if paths.len() > MAX_INCREMENTAL {
            for dir in paths.iter().filter_map(|path| path.parent()).collect::<std::collections::HashSet<_>>() {
                self.emit(dir, &ListingChange::Reload);
            }
            return;
        }
        let mut found = Vec::new();
        for path in paths {
            let Some(dir) = path.parent().map(Path::to_path_buf) else { continue };
            if self.settled(&dir) {
                if let Some(entry) = query_entry(&path).await {
                    found.push((dir, entry));
                }
            }
        }
        let mut deltas: Vec<(PathBuf, Delta)> = Vec::new();
        for (dir, entry) in found {
            let Some(delta) = self.upsert(&dir, entry, adding) else { continue };
            match deltas.iter_mut().find(|(known, _)| *known == dir) {
                Some((_, known)) => known.extend(delta),
                None => deltas.push((dir, delta)),
            }
        }
        for (dir, delta) in deltas {
            self.emit_delta(&dir, delta);
        }
    }

    /// Puts `entry` in `dir`'s listing: in place of the entry for the same path, or, if `add` and there is none, as a
    /// new one. What changed in the listing, if anything.
    fn upsert(&self, dir: &Path, entry: FileEntry, add: bool) -> Option<Delta> {
        let mut listings = self.inner.listings.borrow_mut();
        let listing = listings.get_mut(dir)?;
        if matches!(listing.state, State::Loading) {
            listing.stale = true;
            return None;
        }
        let mut delta = Delta::default();
        match listing.entries.iter().position(|known| known.path() == entry.path()) {
            Some(position) => {
                listing.entries[position] = entry.clone();
                delta.changed.push(entry);
            }
            None if add => {
                listing.entries.push(entry.clone());
                delta.added.push(entry);
            }
            None => return None,
        }
        Some(delta)
    }

    /// Takes `path`'s entry out of `dir`'s listing. Whether there was one.
    fn take_entry(&self, dir: &Path, path: &Path) -> bool {
        let mut listings = self.inner.listings.borrow_mut();
        let Some(listing) = listings.get_mut(dir) else { return false };
        if matches!(listing.state, State::Loading) {
            listing.stale = true;
            return false;
        }
        match listing.entries.iter().position(|known| known.path() == path) {
            Some(position) => {
                listing.entries.remove(position);
                true
            }
            None => false,
        }
    }

    async fn apply_moves(&self, run: Vec<Change>) {
        for change in run {
            let Change::Moved { from, to } = change else { continue };
            let (Some(from_dir), Some(to_dir)) = (from.parent().map(Path::to_path_buf), to.parent().map(Path::to_path_buf))
            else {
                continue;
            };
            // What was listed under a folder that moved is listed under a path that is gone.
            self.clear_under(&from);
            let (from_listed, to_listed) = (self.settled(&from_dir), self.settled(&to_dir));
            if !from_listed && !to_listed {
                continue;
            }
            let Some(entry) = query_entry(&to).await else {
                // Moved away to somewhere that cannot be looked at: for this listing it is gone.
                if self.take_entry(&from_dir, &from) {
                    self.emit(&from_dir, &ListingChange::Removed(vec![from]));
                }
                continue;
            };
            if from_dir == to_dir {
                let had = self.take_entry(&from_dir, &from);
                let Some(delta) = self.upsert(&to_dir, entry.clone(), true) else { continue };
                if had && delta.changed.is_empty() {
                    self.emit(&to_dir, &ListingChange::Moved(vec![(from, entry)]));
                } else {
                    // Moved over a file that was listed: that one is replaced, and the one moved is gone from its name.
                    if had {
                        self.emit(&to_dir, &ListingChange::Removed(vec![from]));
                    }
                    self.emit_delta(&to_dir, delta);
                }
            } else {
                if self.take_entry(&from_dir, &from) {
                    self.emit(&from_dir, &ListingChange::Removed(vec![from]));
                }
                if let Some(delta) = self.upsert(&to_dir, entry, true) {
                    self.emit_delta(&to_dir, delta);
                }
            }
        }
    }

    fn apply_removals(&self, run: Vec<Change>) {
        let mut deltas: Vec<(PathBuf, Delta)> = Vec::new();
        for change in run {
            let (Change::Removed(path) | Change::Unmounted(path)) = change else { continue };
            self.clear_under(&path);
            let Some(dir) = path.parent() else { continue };
            if self.take_entry(dir, &path) {
                match deltas.iter_mut().find(|(known, _)| known == dir) {
                    Some((_, known)) => known.removed.push(path),
                    None => deltas.push((dir.to_path_buf(), Delta { removed: vec![path], ..Delta::default() })),
                }
            }
        }
        for (dir, delta) in deltas {
            self.emit_delta(&dir, delta);
        }
    }

    /// Empties the listings of `path` and of the folders under it, for a folder that is gone or moved.
    fn clear_under(&self, path: &Path) {
        let emptied: Vec<(PathBuf, Vec<PathBuf>)> = {
            let mut listings = self.inner.listings.borrow_mut();
            listings
                .iter_mut()
                .filter(|(dir, listing)| dir.starts_with(path) && !listing.entries.is_empty())
                .map(|(dir, listing)| {
                    let gone = listing.entries.drain(..).map(|entry| entry.path().to_path_buf()).collect();
                    (dir.clone(), gone)
                })
                .collect()
        };
        for (dir, gone) in emptied {
            self.emit(&dir, &ListingChange::Removed(gone));
        }
    }

    /// Whether `dir`'s listing is there and finished, so that a change can be applied to it. A listing being read is
    /// marked, since the read may not have seen the change.
    fn settled(&self, dir: &Path) -> bool {
        let mut listings = self.inner.listings.borrow_mut();
        match listings.get_mut(dir) {
            None => false,
            Some(listing) if matches!(listing.state, State::Loading) => {
                listing.stale = true;
                false
            }
            Some(_) => true,
        }
    }

    fn emit_delta(&self, dir: &Path, delta: Delta) {
        if !delta.removed.is_empty() {
            self.emit(dir, &ListingChange::Removed(delta.removed));
        }
        if !delta.added.is_empty() {
            self.emit(dir, &ListingChange::Added(delta.added));
        }
        if !delta.changed.is_empty() {
            self.emit(dir, &ListingChange::Changed(delta.changed));
        }
    }

    /// Tells the watchers of `dir`'s listing about `change`.
    fn emit(&self, dir: &Path, change: &ListingChange) {
        let watchers = match self.inner.listings.borrow().get(dir) {
            Some(listing) => listing.watchers.clone(),
            None => return,
        };
        for watcher in watchers.iter().filter(|watcher| watcher.active.get()) {
            (watcher.on_change)(change);
        }
    }

    fn next_tick(&self) -> u64 {
        let tick = self.inner.tick.get() + 1;
        self.inner.tick.set(tick);
        tick
    }

    /// Drops the reader `id` from `dir`'s listing. The read is cancelled if that was its last reader.
    fn detach(&self, dir: &Path, id: u64) {
        let cancel = {
            let mut listings = self.inner.listings.borrow_mut();
            let Some(listing) = listings.get_mut(dir) else { return };
            listing.readers.retain(|reader| reader.id != id);
            if listing.readers.is_empty() && matches!(listing.state, State::Loading) {
                Some(listing.cancel.clone())
            } else {
                None
            }
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
            self.inner.listings.borrow_mut().remove(dir);
        }
    }

    /// Adds a batch to `dir`'s listing and gives it to the readers still waiting.
    fn append(&self, dir: &Path, batch: &[FileEntry]) {
        let readers = {
            let mut listings = self.inner.listings.borrow_mut();
            let Some(listing) = listings.get_mut(dir) else { return };
            listing.entries.extend_from_slice(batch);
            listing.readers.clone()
        };
        for reader in readers.iter().filter(|reader| reader.active.get()) {
            (reader.on_batch)(batch);
        }
    }

    /// Ends `dir`'s read: the listing is kept, and the readers are told how it ended.
    fn finish(&self, dir: &Path, result: Result<(), String>) {
        let readers = {
            let mut listings = self.inner.listings.borrow_mut();
            let Some(listing) = listings.get_mut(dir) else { return };
            listing.state = match &result {
                Ok(()) => State::Loaded,
                Err(err) => State::Failed(err.clone()),
            };
            std::mem::take(&mut listing.readers)
        };
        for reader in readers.iter().filter(|reader| reader.active.get()) {
            (reader.on_done)(result.clone());
        }
        // A change that came while the folder was being read may or may not be in what was read.
        let stale = self.inner.listings.borrow_mut().get_mut(dir).is_some_and(|listing| std::mem::take(&mut listing.stale));
        if stale {
            self.emit(dir, &ListingChange::Reload);
        }
        self.trim();
    }

    /// Keeps the finished listings that nobody reads, the most recently used first, up to `MAX_IDLE_LISTINGS` of them
    /// and `MAX_IDLE_ENTRIES` entries in all. The rest are dropped.
    fn trim(&self) {
        let mut listings = self.inner.listings.borrow_mut();
        let mut idle: Vec<(u64, PathBuf, usize)> = listings
            .iter()
            .filter(|(_, listing)| {
                listing.readers.is_empty() && listing.watchers.is_empty() && !matches!(listing.state, State::Loading)
            })
            .map(|(dir, listing)| (listing.used, dir.clone(), listing.entries.len()))
            .collect();
        idle.sort_by(|a, b| b.0.cmp(&a.0));
        let mut kept_entries = 0;
        for (position, (_, dir, count)) in idle.into_iter().enumerate() {
            if position < MAX_IDLE_LISTINGS && kept_entries + count <= MAX_IDLE_ENTRIES {
                kept_entries += count;
            } else {
                listings.remove(&dir);
            }
        }
    }
}

/// Reads `dir` in batches into the store, then ends the read.
async fn read(store: FileStore, dir: PathBuf, cancel: gio::Cancellable) {
    let result = read_batches(&store, &dir, &cancel).await;
    // A cancelled read has no listing left to finish.
    if !cancel.is_cancelled() {
        store.finish(&dir, result.map_err(|err| err.to_string()));
    }
}

async fn read_batches(store: &FileStore, dir: &Path, cancel: &gio::Cancellable) -> Result<(), glib::Error> {
    let enumerator = enumerate(dir, cancel).await?;
    while !cancel.is_cancelled() {
        let infos = next_files(&enumerator, cancel).await?;
        if infos.is_empty() || cancel.is_cancelled() {
            break;
        }
        let batch: Vec<FileEntry> = infos
            .into_iter()
            .map(|info| FileEntry::new(dir.join(info.name()), info))
            .collect();
        store.append(dir, &batch);
    }
    Ok(())
}

/// The enumerator of the children of `dir`. A cancelled read stops in the middle of the I/O, not after it.
async fn enumerate(dir: &Path, cancel: &gio::Cancellable) -> Result<gio::FileEnumerator, glib::Error> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    gio::File::for_path(dir).enumerate_children_async(
        QUERY_ATTRIBUTES,
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
        Some(cancel),
        move |result| {
            let _ = sender.send(result);
        },
    );
    receiver.await.unwrap_or_else(|_| Err(cancelled()))
}

/// The next batch of the children being enumerated, or none at the end.
async fn next_files(enumerator: &gio::FileEnumerator, cancel: &gio::Cancellable) -> Result<Vec<gio::FileInfo>, glib::Error> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    enumerator.next_files_async(BATCH_SIZE, glib::Priority::DEFAULT, Some(cancel), move |result| {
        let _ = sender.send(result);
    });
    receiver.await.unwrap_or_else(|_| Err(cancelled()))
}

/// What a run of changes did to one folder's listing.
#[derive(Default)]
struct Delta {
    added: Vec<FileEntry>,
    changed: Vec<FileEntry>,
    removed: Vec<PathBuf>,
}

impl Delta {
    fn extend(&mut self, other: Delta) {
        self.added.extend(other.added);
        self.changed.extend(other.changed);
        self.removed.extend(other.removed);
    }
}

/// The entry for the file at `path` as it is now, or none if it cannot be looked at.
async fn query_entry(path: &Path) -> Option<FileEntry> {
    let info = gio::File::for_path(path)
        .query_info_future(QUERY_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
        .await
        .ok()?;
    Some(FileEntry::new(path.to_path_buf(), info))
}

fn cancelled() -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::Cancelled, "the read was cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder with `count` files, named so each test has its own.
    fn folder(name: &str, count: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rsimgv-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..count {
            std::fs::write(dir.join(format!("file-{i:04}.txt")), b"x").unwrap();
        }
        dir
    }

    /// Runs `ctx` until `done` holds, and fails if that takes too long.
    fn run_until(ctx: &glib::MainContext, what: &str, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "{what} did not finish");
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn streams_the_folder_in_batches() {
        let dir = folder("batches", 250);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let batches = Rc::new(RefCell::new(Vec::new()));
            let done = Rc::new(RefCell::new(None));
            let store = FileStore::default();
            let _reading = store.read(
                &dir,
                {
                    let batches = batches.clone();
                    move |batch: &[FileEntry]| batches.borrow_mut().push(batch.len())
                },
                {
                    let done = done.clone();
                    move |result| *done.borrow_mut() = Some(result)
                },
            );
            run_until(&ctx, "batched read", || done.borrow().is_some());
            assert_eq!(*batches.borrow(), vec![100, 100, 50]);
            assert!(matches!(done.borrow().as_ref(), Some(Ok(()))));
        })
        .unwrap();
    }

    #[test]
    fn a_second_reader_gets_the_listing_without_reading_again() {
        let dir = folder("replay", 3);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            let first = Rc::new(Cell::new(false));
            let _first = store.read(&dir, |_| {}, {
                let first = first.clone();
                move |_| first.set(true)
            });
            run_until(&ctx, "first read", || first.get());

            let seen = Rc::new(RefCell::new(0));
            let done = Rc::new(Cell::new(false));
            let _second = store.read(
                &dir,
                {
                    let seen = seen.clone();
                    move |batch: &[FileEntry]| *seen.borrow_mut() += batch.len()
                },
                {
                    let done = done.clone();
                    move |_| done.set(true)
                },
            );
            run_until(&ctx, "replayed read", || done.get());
            assert_eq!(*seen.borrow(), 3);
        })
        .unwrap();
    }

    #[test]
    fn a_reader_joining_a_read_in_progress_gets_every_entry_before_the_end() {
        let dir = folder("late-join", 250);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            let joined = Rc::new(RefCell::new(None));
            let seen = Rc::new(RefCell::new(0));
            let at_end = Rc::new(Cell::new(None));
            let done = Rc::new(Cell::new(false));
            let first = {
                let (store, joined, seen, at_end, done) = (store.clone(), joined.clone(), seen.clone(), at_end.clone(), done.clone());
                let batches = Rc::new(Cell::new(0));
                let folder = dir.clone();
                store.clone().read(
                    &dir,
                    move |_batch: &[FileEntry]| {
                        batches.set(batches.get() + 1);
                        // Joins after the first batch, while the rest of the folder is still being read.
                        if batches.get() == 1 && joined.borrow().is_none() {
                            let (seen, at_end, done) = (seen.clone(), at_end.clone(), done.clone());
                            let late = store.read(
                                &folder,
                                {
                                    let seen = seen.clone();
                                    move |batch: &[FileEntry]| *seen.borrow_mut() += batch.len()
                                },
                                move |_| {
                                    at_end.set(Some(*seen.borrow()));
                                    done.set(true);
                                },
                            );
                            *joined.borrow_mut() = Some(late);
                        }
                    },
                    |_| {},
                )
            };
            run_until(&ctx, "late join", || done.get());
            assert_eq!(at_end.get(), Some(250), "every entry arrives before the end is reported");
            drop(first);
        })
        .unwrap();
    }

    #[test]
    fn cancelling_the_last_reader_drops_the_read() {
        let dir = folder("cancel", 250);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            let finished = Rc::new(Cell::new(false));
            let reading = store.read(&dir, |_| {}, {
                let finished = finished.clone();
                move |_| finished.set(true)
            });
            reading.cancel();
            assert!(!store.inner.listings.borrow().contains_key(&dir));
            // Let the cancelled read run to its end; it must not finish a listing that is gone.
            for _ in 0..50 {
                ctx.iteration(false);
            }
            assert!(!finished.get());
            assert!(!store.inner.listings.borrow().contains_key(&dir));
        })
        .unwrap();
    }

    /// Reads `dir` to the end.
    fn list(ctx: &glib::MainContext, store: &FileStore, dir: &Path) {
        let done = Rc::new(Cell::new(false));
        store.read(dir, |_| {}, {
            let done = done.clone();
            move |_| done.set(true)
        });
        run_until(ctx, "read", || done.get());
    }

    /// Watches `dir`, noting what it is told as text like `added:name`.
    fn record(store: &FileStore, dir: &Path) -> (Watching, Rc<RefCell<Vec<String>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let name = |entry: &FileEntry| entry.name();
        let file = |path: &Path| path.file_name().unwrap().to_string_lossy().into_owned();
        let watching = store
            .watch(dir, {
                let seen = seen.clone();
                move |change| {
                    let mut seen = seen.borrow_mut();
                    match change {
                        ListingChange::Added(entries) => seen.extend(entries.iter().map(|e| format!("added:{}", name(e)))),
                        ListingChange::Changed(entries) => seen.extend(entries.iter().map(|e| format!("changed:{}", name(e)))),
                        ListingChange::Removed(paths) => seen.extend(paths.iter().map(|p| format!("removed:{}", file(p)))),
                        ListingChange::Moved(pairs) => {
                            seen.extend(pairs.iter().map(|(old, new)| format!("moved:{}>{}", file(old), name(new))))
                        }
                        ListingChange::Reload => seen.push("reload".to_string()),
                    }
                }
            })
            .expect("the folder is listed");
        (watching, seen)
    }

    /// The names in `dir`'s listing, in order.
    fn listed(store: &FileStore, dir: &Path) -> Vec<String> {
        let listings = store.inner.listings.borrow();
        let mut names: Vec<String> = listings[dir].entries.iter().map(|entry| entry.name()).collect();
        names.sort();
        names
    }

    /// Runs `ctx` until the applying of changes is over.
    fn settle(ctx: &glib::MainContext, store: &FileStore) {
        run_until(ctx, "changes", || !store.inner.applying.get() && store.inner.pending.borrow().is_empty());
    }

    #[test]
    fn an_added_file_joins_the_listing_and_is_told_to_watchers() {
        let dir = folder("added", 2);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            std::fs::write(dir.join("new.txt"), b"x").unwrap();
            store.notify(vec![Change::Added(dir.join("new.txt"))]);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["added:new.txt"]);
            assert_eq!(listed(&store, &dir), ["file-0000.txt", "file-0001.txt", "new.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn a_changed_file_is_looked_at_again_and_an_unknown_one_is_ignored() {
        let dir = folder("changed", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            std::fs::write(dir.join("file-0000.txt"), b"longer now").unwrap();
            std::fs::write(dir.join("stranger.txt"), b"x").unwrap();
            store.notify(vec![
                Change::Changed(dir.join("file-0000.txt")),
                Change::Changed(dir.join("stranger.txt")),
            ]);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["changed:file-0000.txt"]);
            let size = store.inner.listings.borrow()[&dir].entries[0].size();
            assert_eq!(size, 10);
            assert_eq!(listed(&store, &dir), ["file-0000.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn a_removed_file_leaves_the_listing() {
        let dir = folder("removed", 3);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            std::fs::remove_file(dir.join("file-0001.txt")).unwrap();
            store.notify(vec![Change::Removed(dir.join("file-0001.txt")), Change::Removed(dir.join("never-listed"))]);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["removed:file-0001.txt"]);
            assert_eq!(listed(&store, &dir), ["file-0000.txt", "file-0002.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn an_addition_and_the_removal_after_it_leave_nothing_behind() {
        let dir = folder("order", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            // The file is already gone when the addition is looked at, and the removal is applied after it.
            store.notify(vec![Change::Added(dir.join("brief.txt")), Change::Removed(dir.join("brief.txt"))]);
            settle(&ctx, &store);
            assert_eq!(listed(&store, &dir), ["file-0000.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn a_file_renamed_in_its_folder_is_one_move() {
        let dir = folder("rename", 2);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            std::fs::rename(dir.join("file-0000.txt"), dir.join("renamed.txt")).unwrap();
            store.notify(vec![Change::Moved { from: dir.join("file-0000.txt"), to: dir.join("renamed.txt") }]);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["moved:file-0000.txt>renamed.txt"]);
            assert_eq!(listed(&store, &dir), ["file-0001.txt", "renamed.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn a_file_moved_between_listed_folders_leaves_one_and_joins_the_other() {
        let from = folder("move-from", 2);
        let to = folder("move-to", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &from);
            list(&ctx, &store, &to);
            let (_from_watching, left) = record(&store, &from);
            let (_to_watching, joined) = record(&store, &to);
            std::fs::rename(from.join("file-0000.txt"), to.join("arrived.txt")).unwrap();
            store.notify(vec![Change::Moved { from: from.join("file-0000.txt"), to: to.join("arrived.txt") }]);
            settle(&ctx, &store);
            assert_eq!(*left.borrow(), ["removed:file-0000.txt"]);
            assert_eq!(*joined.borrow(), ["added:arrived.txt"]);
            assert_eq!(listed(&store, &from), ["file-0001.txt"]);
            assert_eq!(listed(&store, &to), ["arrived.txt", "file-0000.txt"]);
        })
        .unwrap();
    }

    #[test]
    fn a_folder_that_is_gone_empties_its_listing() {
        let parent = folder("gone", 0);
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("inside.txt"), b"x").unwrap();
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &parent);
            list(&ctx, &store, &child);
            let (_watching, seen) = record(&store, &child);
            std::fs::remove_dir_all(&child).unwrap();
            store.notify(vec![Change::Removed(child.clone())]);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["removed:inside.txt"]);
            assert!(listed(&store, &child).is_empty());
            assert!(listed(&store, &parent).is_empty());
        })
        .unwrap();
    }

    #[test]
    fn too_many_changes_to_look_at_one_by_one_ask_for_a_reload() {
        let dir = folder("flood", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            let flood = (0..=MAX_INCREMENTAL).map(|i| Change::Added(dir.join(format!("flood-{i}")))).collect();
            store.notify(flood);
            settle(&ctx, &store);
            assert_eq!(*seen.borrow(), ["reload"]);
        })
        .unwrap();
    }

    #[test]
    fn a_change_during_a_read_never_doubles_an_entry() {
        let dir = folder("during", 250);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            let done = Rc::new(Cell::new(false));
            store.read(&dir, |_| {}, {
                let done = done.clone();
                move |_| done.set(true)
            });
            let (_watching, seen) = record(&store, &dir);
            std::fs::write(dir.join("late.txt"), b"x").unwrap();
            store.notify(vec![Change::Added(dir.join("late.txt"))]);
            run_until(&ctx, "read", || done.get());
            settle(&ctx, &store);
            let late = listed(&store, &dir).iter().filter(|name| *name == "late.txt").count();
            assert!(late <= 1, "late.txt is listed {late} times");
            // Either the change was applied to a finished listing, or the read may have missed it and a reload is asked for.
            assert!(late == 1 || seen.borrow().iter().any(|told| told == "reload"));
        })
        .unwrap();
    }

    #[test]
    fn a_watched_listing_is_kept_and_an_unwatched_one_is_not() {
        let dir = folder("kept", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::default();
            list(&ctx, &store, &dir);
            let (watching, _seen) = record(&store, &dir);
            store.discard_idle(&dir);
            assert!(store.inner.listings.borrow().contains_key(&dir));
            assert!(store.inner.listings.borrow()[&dir].monitor.is_some());
            watching.cancel();
            assert!(store.inner.listings.borrow()[&dir].monitor.is_none());
            store.discard_idle(&dir);
            assert!(!store.inner.listings.borrow().contains_key(&dir));
        })
        .unwrap();
    }

    #[test]
    fn the_monitor_carries_a_real_change_through_to_the_watchers() {
        let dir = folder("monitor", 1);
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let store = FileStore::shared();
            list(&ctx, &store, &dir);
            let (_watching, seen) = record(&store, &dir);
            assert!(store.inner.listings.borrow()[&dir].monitor.as_ref().unwrap().is_watching());
            std::fs::write(dir.join("fresh.txt"), b"x").unwrap();
            run_until(&ctx, "the monitor's report", || seen.borrow().iter().any(|told| told == "added:fresh.txt"));
            std::fs::rename(dir.join("fresh.txt"), dir.join("fresher.txt")).unwrap();
            run_until(&ctx, "the monitor's report of the rename", || {
                seen.borrow().iter().any(|told| told == "moved:fresh.txt>fresher.txt")
            });
            std::fs::remove_file(dir.join("fresher.txt")).unwrap();
            run_until(&ctx, "the monitor's report of the removal", || {
                seen.borrow().iter().any(|told| told == "removed:fresher.txt")
            });
            assert_eq!(listed(&store, &dir), ["file-0000.txt"]);
        })
        .unwrap();
    }
}
