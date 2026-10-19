//! The app's shared store of folder listings. One `FileStore` serves the whole process (the folder
//! tree, the browse view, ...), so a folder is read once and every reader gets the same `FileEntry`
//! objects.
//!
//! Like Nautilus's directory loading, a read is streamed: readers are given each batch of entries as it
//! arrives. A reader that joins a read already in progress is first given the entries read so far. A
//! read that no reader is waiting for any more is cancelled.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use gtk::{gio, glib};

use super::FileEntry;

/// The attributes read for every entry.
const QUERY_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size,\
standard::icon,standard::content-type,standard::is-hidden,time::modified";
/// Entries requested per batch, as Nautilus does.
const BATCH_SIZE: i32 = 100;
/// Finished listings that nobody reads are kept this many at most; the least recently used go first.
const MAX_IDLE_LISTINGS: usize = 16;

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
            // Replayed on the thread's main context, like the reads themselves.
            glib::spawn_future_local(async move {
                if !reader.active.get() {
                    return;
                }
                if !entries.is_empty() {
                    (reader.on_batch)(&entries);
                }
                if let Some(result) = done {
                    (reader.on_done)(result);
                }
            });
        }

        Reading {
            store: self.clone(),
            dir: dir.to_path_buf(),
            id,
            active,
        }
    }

    /// Forgets `dir`'s listing if it has finished and nobody is reading it, so the next read lists the
    /// folder afresh.
    pub fn discard_idle(&self, dir: &Path) {
        let mut listings = self.inner.listings.borrow_mut();
        let idle = listings
            .get(dir)
            .is_some_and(|listing| listing.readers.is_empty() && !matches!(listing.state, State::Loading));
        if idle {
            listings.remove(dir);
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
        self.trim();
    }

    /// Keeps at most `MAX_IDLE_LISTINGS` finished listings that nobody reads, dropping the least recently used.
    fn trim(&self) {
        let mut listings = self.inner.listings.borrow_mut();
        let mut idle: Vec<(u64, PathBuf)> = listings
            .iter()
            .filter(|(_, listing)| listing.readers.is_empty() && !matches!(listing.state, State::Loading))
            .map(|(dir, listing)| (listing.used, dir.clone()))
            .collect();
        if idle.len() <= MAX_IDLE_LISTINGS {
            return;
        }
        idle.sort();
        for (_, dir) in idle.drain(..idle.len() - MAX_IDLE_LISTINGS) {
            listings.remove(&dir);
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
    let enumerator = gio::File::for_path(dir)
        .enumerate_children_future(QUERY_ATTRIBUTES, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
        .await?;
    while !cancel.is_cancelled() {
        let infos = enumerator.next_files_future(BATCH_SIZE, glib::Priority::DEFAULT).await?;
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
}
