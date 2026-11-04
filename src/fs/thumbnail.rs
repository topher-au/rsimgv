//! Thumbnails of pictures and videos, kept in the app's own cache (`~/.cache/rsimgv`). A thumbnail is made
//! on a worker thread at the smallest size that covers the item, and written as a PNG. A picture whose
//! thumbnail is current for its modification time is not decoded again. With the cache turned off, nothing
//! is read from it or written to it, and thumbnails last only as long as they are in memory.
//!
//! A video's still thumbnail is a frame a tenth of the way in. Hovering a video asks for a sequence of up
//! to `MAX_FRAMES` frames spread through it, cached beside the thumbnails. Frames come from the `ffmpeg`
//! and `ffprobe` programs; without them, videos keep their icon.
//!
//! Like Nautilus's thumbnailer, only a few are made at a time, and a request that no item on screen
//! wants any more is dropped before it runs, or stopped if it already runs. How far from the screen items are still
//! wanted is set by the `Aggression` level: from only the items on screen, to the whole folder. Items off screen are asked
//! for with `prefetch_still` and wait for everything on screen, which can stop one of them to start at once. A thumbnail no item on screen shows any more can be let go from
//! memory too, and is read back when it is next wanted. They are made on a thread pool of their own, so however many
//! run, they leave GIO's few worker threads to folder listings.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk::glib::object::Cast;
use gtk::prelude::PaintableExt;
use gtk::{gdk, gdk_pixbuf, gio, glib};

use crate::hash::string_hash;

/// Thumbnails made at the same time, unless the settings say otherwise.
const DEFAULT_THREADS: usize = 4;
/// Bytes of thumbnails kept in memory; the oldest go first. A 2048-pixel thumbnail alone is 16 MB.
const MAX_BYTES: usize = 256 * 1024 * 1024;
/// The sizes thumbnails are made at, in pixels. An item is shown from the smallest one that covers it.
pub const SIZES: [i32; 7] = [32, 64, 128, 256, 512, 1024, 2048];
/// Most frames in a video's hover sequence.
pub const MAX_FRAMES: usize = 10;
/// Frame jobs made at the same time, so that they leave room for stills.
const MAX_FRAME_JOBS: usize = 1;
/// How long `ffmpeg` or `ffprobe` may run on one file before it is stopped.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// A thumbnail size, in pixels, one of `SIZES`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Size(i32);

impl Default for Size {
    /// The largest size, so that nothing is capped.
    fn default() -> Self {
        Self(SIZES[SIZES.len() - 1])
    }
}

impl Size {
    /// The smallest size that covers `pixels`: the next size up, which is then scaled down for the item.
    pub fn for_pixels(pixels: i32) -> Self {
        Self(SIZES.iter().copied().find(|&size| size >= pixels).unwrap_or(SIZES[SIZES.len() - 1]))
    }

    /// The largest size that is at most `pixels`, or the smallest one if `pixels` is below all of them.
    pub fn at_most(pixels: i32) -> Self {
        Self(SIZES.iter().copied().rev().find(|&size| size <= pixels).unwrap_or(SIZES[0]))
    }

    pub fn pixels(self) -> i32 {
        self.0
    }

    fn dir_name(self) -> String {
        self.0.to_string()
    }
}

/// How eagerly thumbnails are made for items that are not on screen.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Aggression {
    /// Only the items on screen. A job whose item scrolls out of view is dropped, and stopped if it is running.
    Low,
    /// Also the items up to a page away from those on screen.
    #[default]
    Standard,
    /// Items up to two pages away, and making thumbnails goes on while a video plays.
    Increased,
    /// Items up to four pages away.
    High,
    /// Every item in the folder shown. What comes into view is made first, ahead of the rest.
    Extreme,
}

impl Aggression {
    pub const ALL: [Aggression; 5] = [Aggression::Low, Aggression::Standard, Aggression::Increased, Aggression::High, Aggression::Extreme];

    pub fn name(self) -> &'static str {
        match self {
            Aggression::Low => "low",
            Aggression::Standard => "standard",
            Aggression::Increased => "increased",
            Aggression::High => "high",
            Aggression::Extreme => "extreme",
        }
    }

    /// The level a setting value names, or the standard one for any other.
    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|level| level.name() == name).unwrap_or_default()
    }

    /// How many pages away from the items on screen thumbnails are made for, or `None` for the whole folder.
    pub fn pages(self) -> Option<f64> {
        match self {
            Aggression::Low => Some(0.0),
            Aggression::Standard => Some(1.0),
            Aggression::Increased => Some(2.0),
            Aggression::High => Some(4.0),
            Aggression::Extreme => None,
        }
    }

    /// Whether making thumbnails goes on while a video plays, which can slow the video.
    pub fn works_during_playback(self) -> bool {
        self >= Aggression::Increased
    }
}

/// What a thumbnail is of.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Media {
    Picture,
    Video,
}

/// What a job makes: one still thumbnail, or the frames of a video's hover sequence.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Job {
    Still(Media),
    Frames,
}

/// One thing to make: the file, its modification time in seconds, the size class, and what to make.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    mtime: i64,
    size: Size,
    job: Job,
}

/// Receives the textures of a finished job.
type OnReady = dyn FnOnce(&[gdk::Texture]);

/// Says whether a request for a thumbnail, or the item it was given to, is still on screen.
type Wanted = Rc<dyn Fn() -> bool>;

/// A request for a thumbnail. `wanted` says whether it is still needed.
struct Waiter {
    wanted: Wanted,
    /// Whether it is for something on screen, which is made ahead of what is only being made ready.
    urgent: bool,
    /// For what is only being made ready: how far the item is from the screen now, the nearest being made first.
    rank: Option<Rc<dyn Fn() -> f64>>,
    on_ready: Box<OnReady>,
}

/// What has been made, by job.
type TextureCache = HashMap<Key, Vec<gdk::Texture>>;

#[derive(Default)]
struct Inner {
    root: PathBuf,
    /// What has been made in this process, by file.
    textures: RefCell<TextureCache>,
    /// The order the cached things were used in, with the stamp each had then, for evicting the least recently used.
    /// An entry whose stamp is no longer the thing's own is stale and is skipped.
    used: RefCell<VecDeque<(Key, u64)>>,
    /// The stamp of the latest use of each cached thing.
    stamps: RefCell<HashMap<Key, u64>>,
    /// The last stamp handed out.
    stamp: Cell<u64>,
    /// The bytes the textures in `textures` take up.
    bytes: Cell<usize>,
    /// Requests waiting for each thing, whether it is queued or being made.
    waiters: RefCell<HashMap<Key, Vec<Waiter>>>,
    /// Jobs waiting to be made, newest last, each with its stamp. `queued` holds the stamp that is current for each,
    /// so asking for a queued job again moves it to the newest end without searching the queue.
    queue: RefCell<VecDeque<(Key, u64)>>,
    /// The same for jobs that are only being made ready for items that are not on screen, which wait for all the others.
    queue_low: RefCell<Vec<(Key, u64)>>,
    queued: RefCell<HashMap<Key, u64>>,
    running: Cell<usize>,
    /// The running jobs, each with the flag that stops it.
    cancels: RefCell<HashMap<Key, Arc<AtomicBool>>>,
    /// The largest size pictures' and videos' thumbnails are made at. A request for a larger size is made at this one.
    max_image: Cell<Size>,
    max_video: Cell<Size>,
    /// How eagerly thumbnails are made for items off screen, and whether a video is playing.
    aggression: Cell<Aggression>,
    playing: Cell<bool>,
    /// Whether the timer that stops jobs nothing wants is running.
    watching: Cell<bool>,
    /// What to call when the level is set.
    aggression_watchers: RefCell<Vec<Rc<dyn Fn()>>>,
    /// How many jobs may run at the same time.
    max_jobs: Cell<usize>,
    /// The threads that make the jobs.
    pool: Option<glib::ThreadPool>,
    /// How many of the running jobs are frame jobs.
    running_frames: Cell<usize>,
    /// Whether the cache under `root` is used.
    disk_cache: Cell<bool>,
    /// Whether things that no item shows any more are let go from `textures`.
    unload_offscreen: Cell<bool>,
    /// What each thing was given to, for as long as that is still on screen.
    holders: RefCell<HashMap<Key, Vec<Wanted>>>,
    /// Whether a pass over `holders` is waiting to run.
    unload_pending: Cell<bool>,
    /// The things that could not be made, so they are not tried again.
    failed: RefCell<HashSet<Key>>,
}

/// How long after an item leaves the screen its thumbnail is let go, so a quick scroll back finds it still there.
const UNLOAD_DELAY: Duration = Duration::from_secs(1);

/// The most holders remembered for one thumbnail.
const MAX_HOLDERS: usize = 8;

/// How many days a thumbnail may go unmade before the start-up sweep removes it. It is made again when it is needed.
const KEEP_DAYS: u64 = 90;

thread_local! {
    static SHARED: Thumbnails = start_shared();
}

/// The app's thumbnails in the user's cache, with the sweep of stale ones started in the background.
fn start_shared() -> Thumbnails {
    let root = glib::user_cache_dir().join("rsimgv");
    sweep(root.clone());
    Thumbnails::with_root(root)
}

/// Removes, in the background, the thumbnails under `root` that have not been made for `KEEP_DAYS`, and the temporary
/// files that writes left behind.
fn sweep(root: PathBuf) {
    std::thread::spawn(move || {
        let Some(oldest) = std::time::SystemTime::now().checked_sub(Duration::from_secs(KEEP_DAYS * 24 * 60 * 60)) else {
            return;
        };
        let mut folders = vec![root];
        while let Some(folder) = folders.pop() {
            let Ok(read) = std::fs::read_dir(&folder) else { continue };
            for entry in read.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    folders.push(path);
                    continue;
                }
                let temporary = path.extension().is_some_and(|extension| extension == "tmp");
                let stale = entry.metadata().and_then(|metadata| metadata.modified()).is_ok_and(|modified| modified < oldest);
                if temporary || stale {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    });
}

/// The thumbnails of the app: one per process, like the file store.
#[derive(Clone)]
pub struct Thumbnails {
    inner: Rc<Inner>,
}

impl Thumbnails {
    /// The thumbnails of this app, in the user's thumbnail cache.
    pub fn shared() -> Self {
        SHARED.with(Self::clone)
    }

    fn with_root(root: PathBuf) -> Self {
        Self {
            inner: Rc::new(Inner {
                root,
                max_jobs: Cell::new(DEFAULT_THREADS.min(thread_limit())),
                disk_cache: Cell::new(true),
                unload_offscreen: Cell::new(true),
                pool: Some(glib::ThreadPool::shared(None).expect("a shared thread pool does not start any threads yet")),
                ..Inner::default()
            }),
        }
    }

    /// Makes up to `threads` thumbnails at the same time, at most the number of threads on the system. Jobs already
    /// running over a lower count finish; a higher count starts the queued ones now.
    pub fn set_thread_count(&self, threads: usize) {
        self.inner.max_jobs.set(threads.clamp(1, thread_limit()));
        self.start_jobs();
    }

    /// Sets the largest size thumbnails of `media` are made at. An item still asks for the size its own size needs, but
    /// the thumbnail is made no larger than `max`, and is scaled up or down by the item as it is shown.
    pub fn set_max_size(&self, media: Media, max: Size) {
        match media {
            Media::Picture => self.inner.max_image.set(max),
            Media::Video => self.inner.max_video.set(max),
        }
    }

    /// `size`, capped at the largest size set for thumbnails of `media`.
    fn capped(&self, size: Size, media: Media) -> Size {
        let max = match media {
            Media::Picture => self.inner.max_image.get(),
            Media::Video => self.inner.max_video.get(),
        };
        size.min(max)
    }

    /// Sets how eagerly thumbnails are made for items that are not on screen. Jobs nothing wants any more are stopped, and
    /// the others go on.
    pub fn set_aggression(&self, level: Aggression) {
        let changed = self.inner.aggression.replace(level) != level;
        self.stop_unwanted();
        self.start_jobs();
        if changed {
            let watchers = self.inner.aggression_watchers.borrow().clone();
            for watcher in watchers {
                watcher();
            }
        }
    }

    /// Calls `callback` whenever the level is set, so what asks for thumbnails ahead of time can ask again.
    pub fn watch_aggression(&self, callback: impl Fn() + 'static) {
        self.inner.aggression_watchers.borrow_mut().push(Rc::new(callback));
    }

    pub fn aggression(&self) -> Aggression {
        self.inner.aggression.get()
    }

    /// Notes whether a video is playing. Below `Increased`, nothing is made then, and what is running is stopped and put
    /// back in the queue.
    pub fn set_media_playing(&self, playing: bool) {
        self.inner.playing.set(playing);
        self.stop_unwanted();
        self.start_jobs();
    }

    /// Whether making thumbnails waits, because a video is playing.
    fn paused(&self) -> bool {
        self.inner.playing.get() && !self.inner.aggression.get().works_during_playback()
    }

    /// Stops the running jobs that nothing wants any more, or all of them while making thumbnails is paused. A stopped job
    /// that something still wants goes back in the queue.
    fn stop_unwanted(&self) {
        let paused = self.paused();
        let stop: Vec<Arc<AtomicBool>> = {
            let cancels = self.inner.cancels.borrow();
            let waiters = self.inner.waiters.borrow();
            cancels
                .iter()
                .filter(|(key, _)| paused || !waiters.get(*key).is_some_and(|list| list.iter().any(|waiter| (waiter.wanted)())))
                .map(|(_, flag)| flag.clone())
                .collect()
        };
        for flag in stop {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Looks every moment for running jobs that nothing wants any more, while any run, and stops them.
    fn watch_running(&self) {
        if self.inner.watching.replace(true) {
            return;
        }
        let thumbnails = self.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if thumbnails.inner.running.get() == 0 {
                thumbnails.inner.watching.set(false);
                return glib::ControlFlow::Break;
            }
            thumbnails.stop_unwanted();
            glib::ControlFlow::Continue
        });
    }

    /// Sets whether thumbnails are read from and written to the disk. What is already there is left alone: it is
    /// current again if the cache is turned back on, and the start-up sweep removes it in time.
    pub fn set_disk_cache(&self, on: bool) {
        self.inner.disk_cache.set(on);
    }

    /// Sets whether thumbnails that no item shows any more are let go from memory. Otherwise they stay until the memory
    /// budget pushes them out.
    pub fn set_unload_offscreen(&self, on: bool) {
        self.inner.unload_offscreen.set(on);
        if on {
            self.unload_soon();
        }
    }

    /// Whether thumbnails that no item shows any more are let go from memory, so items leaving the screen should let
    /// go of theirs too.
    pub fn unloads_offscreen(&self) -> bool {
        self.inner.unload_offscreen.get()
    }

    /// Looks, in a moment, for thumbnails that no item shows any more. Called when an item leaves the screen.
    pub fn unload_soon(&self) {
        if self.inner.unload_pending.replace(true) {
            return;
        }
        let thumbnails = self.clone();
        glib::timeout_add_local_once(UNLOAD_DELAY, move || {
            thumbnails.inner.unload_pending.set(false);
            thumbnails.unload_offscreen();
        });
    }

    /// Forgets what is no longer on screen, and lets go of the thumbnails that leaves with nothing to show them.
    fn unload_offscreen(&self) {
        self.inner.holders.borrow_mut().retain(|_, list| {
            list.retain(|wanted| wanted());
            !list.is_empty()
        });
        if !self.inner.unload_offscreen.get() {
            return;
        }
        let gone: Vec<Key> = {
            let holders = self.inner.holders.borrow();
            self.inner.textures.borrow().keys().filter(|key| !holders.contains_key(*key)).cloned().collect()
        };
        for key in gone {
            self.forget(&key);
        }
    }

    /// Lets go of the cached thing `key`.
    fn forget(&self, key: &Key) {
        self.inner.stamps.borrow_mut().remove(key);
        if let Some(removed) = self.inner.textures.borrow_mut().remove(key) {
            self.inner.bytes.set(self.inner.bytes.get() - bytes_of(&removed));
        }
    }

    /// Remembers that `key` was given to something, which holds on to it while `wanted` says so. A folder that keeps
    /// cycling asks for the same pictures again and again, so only the latest few holders are kept. Losing one costs
    /// at most reading the thumbnail back.
    fn hold(&self, key: &Key, wanted: Wanted) {
        let mut holders = self.inner.holders.borrow_mut();
        let list = holders.entry(key.clone()).or_default();
        list.retain(|wanted| wanted());
        if list.len() >= MAX_HOLDERS {
            list.remove(0);
        }
        list.push(wanted);
    }

    /// Whether a thumbnail of the file at `path` is in memory.
    #[cfg(test)]
    pub fn has_loaded(&self, path: &Path) -> bool {
        self.inner.textures.borrow().keys().any(|key| key.path == path)
    }

    /// The largest still thumbnail of the file at `path`, modified at `mtime`, that has already been made: in memory, or in the
    /// cache on the disk. Nothing is made for it, so there is none for a file that has not been thumbnailed.
    pub async fn best_existing(&self, path: &Path, mtime: i64) -> Option<gdk::Texture> {
        let in_memory = {
            let textures = self.inner.textures.borrow();
            textures
                .iter()
                .filter(|(key, _)| key.path == path && key.mtime == mtime && matches!(key.job, Job::Still(_)))
                .max_by_key(|(key, _)| key.size.pixels())
                .and_then(|(key, textures)| textures.first().map(|texture| (key.size.pixels(), texture.clone())))
        };
        let above = in_memory.as_ref().map_or(0, |(size, _)| *size);
        if self.inner.disk_cache.get() {
            let (root, file) = (self.inner.root.clone(), path.to_path_buf());
            // Only a size larger than the one in memory is worth reading.
            let found = gio::spawn_blocking(move || largest_on_disk(&root, &file, mtime, above)).await.ok().flatten();
            if let Some(pixels) = found {
                return Some(pixels.texture());
            }
        }
        in_memory.map(|(_, texture)| texture)
    }

    /// Calls `on_ready` with the still thumbnail of the file at `path`, modified at `mtime`, once it is made.
    /// A thumbnail already made is given at once. Nothing is called if it cannot be made.
    pub fn request_still(
        &self,
        path: &Path,
        mtime: i64,
        media: Media,
        size: Size,
        wanted: impl Fn() -> bool + 'static,
        on_ready: impl FnOnce(&gdk::Texture) + 'static,
    ) {
        self.request_still_or_failed(path, mtime, media, size, wanted, move |texture| {
            if let Some(texture) = texture {
                on_ready(texture);
            }
        });
    }

    /// Like `request_still`, but `on_done` is also called with `None` when the thumbnail cannot be made, so the
    /// caller can count it as done.
    pub fn request_still_or_failed(
        &self,
        path: &Path,
        mtime: i64,
        media: Media,
        size: Size,
        wanted: impl Fn() -> bool + 'static,
        on_done: impl FnOnce(Option<&gdk::Texture>) + 'static,
    ) {
        let key = key(path, mtime, self.capped(size, media), Job::Still(media));
        self.request(key, None, wanted, move |textures| on_done(textures.first()));
    }

    /// Makes the still thumbnail of the file at `path` ready, for an item that is not on screen. It waits for every
    /// request from something on screen, and is stopped and dropped once `wanted` says no.
    /// `rank` says how far the item is from the screen now: the nearest is made first.
    pub fn prefetch_still(
        &self,
        path: &Path,
        mtime: i64,
        media: Media,
        size: Size,
        rank: impl Fn() -> f64 + 'static,
        wanted: impl Fn() -> bool + 'static,
        on_ready: impl FnOnce(&gdk::Texture) + 'static,
    ) {
        self.request(key(path, mtime, self.capped(size, media), Job::Still(media)), Some(Rc::new(rank)), wanted, move |textures| {
            if let Some(texture) = textures.first() {
                on_ready(texture);
            }
        });
    }

    /// Calls `on_ready` with the frames of the video at `path`, spread through it, once they are made.
    pub fn request_frames(
        &self,
        path: &Path,
        mtime: i64,
        size: Size,
        wanted: impl Fn() -> bool + 'static,
        on_ready: impl FnOnce(&[gdk::Texture]) + 'static,
    ) {
        self.request(key(path, mtime, self.capped(size, Media::Video), Job::Frames), None, wanted, on_ready);
    }

    fn request(&self, key: Key, rank: Option<Rc<dyn Fn() -> f64>>, wanted: impl Fn() -> bool + 'static, on_ready: impl FnOnce(&[gdk::Texture]) + 'static) {
        let wanted: Wanted = Rc::new(wanted);
        let cached = self.inner.textures.borrow().get(&key).cloned();
        if let Some(textures) = cached {
            self.touch(&key);
            self.hold(&key, wanted);
            on_ready(&textures);
            return;
        }
        if self.inner.failed.borrow().contains(&key) {
            on_ready(&[]);
            return;
        }
        let waiter = Waiter {
            wanted,
            urgent: rank.is_none(),
            rank,
            on_ready: Box::new(on_ready),
        };
        let mut waiters = self.inner.waiters.borrow_mut();
        if let Some(list) = waiters.get_mut(&key) {
            // Requests that no longer want it are dropped first, so the list does not grow each time a cell asks again.
            list.retain(|waiter| (waiter.wanted)());
            list.push(waiter);
            let urgent = list.iter().any(|waiter| waiter.urgent);
            drop(waiters);
            // Asked for again while queued, so it is wanted now: it moves to the front of the queue.
            if self.inner.queued.borrow().contains_key(&key) {
                self.enqueue(key, urgent);
                self.make_room_for(urgent);
            }
            return;
        }
        let urgent = waiter.urgent;
        waiters.insert(key.clone(), vec![waiter]);
        drop(waiters);
        self.enqueue(key, urgent);
        self.make_room_for(urgent);
        self.start_jobs();
    }

    /// When something on screen is waiting and every job slot is taken, stops a job that is only making something ready
    /// for an item off screen, so the one on screen starts at once. The stopped job goes back in the queue.
    fn make_room_for(&self, urgent: bool) {
        if !urgent || self.inner.running.get() < self.inner.max_jobs.get() {
            return;
        }
        let victim = {
            let waiters = self.inner.waiters.borrow();
            let cancels = self.inner.cancels.borrow();
            cancels.iter().find(|(key, _)| waiters.get(*key).is_none_or(|list| list.iter().all(|waiter| !waiter.urgent))).map(|(_, flag)| flag.clone())
        };
        if let Some(flag) = victim {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Puts `key` at the newest end of the queue for jobs for things on screen, or in the queue for the rest.
    fn enqueue(&self, key: Key, urgent: bool) {
        let stamp = self.next_stamp();
        self.inner.queued.borrow_mut().insert(key.clone(), stamp);
        let live = self.inner.queued.borrow().len();
        if urgent {
            let mut queue = self.inner.queue.borrow_mut();
            queue.push_back((key, stamp));
            // Moved jobs leave stale entries behind; they are cleared out once they outnumber the live ones.
            if queue.len() > 2 * live + 64 {
                let queued = self.inner.queued.borrow();
                queue.retain(|(key, stamp)| queued.get(key) == Some(stamp));
            }
        } else {
            let mut queue = self.inner.queue_low.borrow_mut();
            queue.push((key, stamp));
            if queue.len() > 2 * live + 64 {
                let queued = self.inner.queued.borrow();
                queue.retain(|(key, stamp)| queued.get(key) == Some(stamp));
            }
        }
    }

    /// How far from the screen the item of the queued job `key` is now: the nearest of those waiting for it.
    fn rank_of(&self, key: &Key) -> f64 {
        let waiters = self.inner.waiters.borrow();
        waiters
            .get(key)
            .into_iter()
            .flatten()
            .filter(|waiter| (waiter.wanted)())
            .filter_map(|waiter| waiter.rank.as_ref().map(|rank| rank()))
            .min_by(f64::total_cmp)
            .unwrap_or(f64::MAX)
    }

    fn next_stamp(&self) -> u64 {
        let stamp = self.inner.stamp.get() + 1;
        self.inner.stamp.set(stamp);
        stamp
    }

    /// The newest queued job for something on screen with its stamp, or else the one for the item nearest the screen, skipping
    /// stale entries.
    fn pop_newest(&self) -> Option<(Key, u64)> {
        loop {
            let Some((key, stamp)) = self.inner.queue.borrow_mut().pop_back() else { break };
            if self.inner.queued.borrow().get(&key) == Some(&stamp) {
                return Some((key, stamp));
            }
        }
        let mut queue = self.inner.queue_low.borrow_mut();
        {
            let queued = self.inner.queued.borrow();
            queue.retain(|(key, stamp)| queued.get(key) == Some(stamp));
        }
        let nearest = queue.iter().enumerate().min_by(|a, b| self.rank_of(&a.1 .0).total_cmp(&self.rank_of(&b.1 .0))).map(|(index, _)| index)?;
        Some(queue.swap_remove(nearest))
    }

    /// Marks the cached thing `key` as the most recently used.
    fn touch(&self, key: &Key) {
        let stamp = self.next_stamp();
        self.inner.stamps.borrow_mut().insert(key.clone(), stamp);
        let mut used = self.inner.used.borrow_mut();
        used.push_back((key.clone(), stamp));
        if used.len() > 2 * self.inner.textures.borrow().len() + 64 {
            let stamps = self.inner.stamps.borrow();
            used.retain(|(key, stamp)| stamps.get(key) == Some(stamp));
        }
    }

    /// Starts queued jobs while there is room for them.
    fn start_jobs(&self) {
        while self.inner.running.get() < self.inner.max_jobs.get() && !self.paused() {
            let Some(key) = self.next_wanted() else { return };
            self.inner.running.set(self.inner.running.get() + 1);
            if key.job == Job::Frames {
                self.inner.running_frames.set(self.inner.running_frames.get() + 1);
            }
            let cancel = Arc::new(AtomicBool::new(false));
            self.inner.cancels.borrow_mut().insert(key.clone(), cancel.clone());
            self.watch_running();
            glib::spawn_future_local(run(self.clone(), key, cancel));
        }
    }

    /// The newest queued job that something on screen still wants. The queue is newest first: items
    /// that just came into view are requested last, so they are made first. A job nothing wants any
    /// more, because its item has scrolled away, is dropped without being made. Frame jobs take at most
    /// `MAX_FRAME_JOBS` of the slots, so the stills of items on screen are not held up behind them.
    fn next_wanted(&self) -> Option<Key> {
        let frames_full = self.inner.running_frames.get() >= MAX_FRAME_JOBS;
        let mut skipped_frames = Vec::new();
        let found = loop {
            let Some((key, stamp)) = self.pop_newest() else { break None };
            if frames_full && key.job == Job::Frames {
                skipped_frames.push((key, stamp));
                continue;
            }
            self.inner.queued.borrow_mut().remove(&key);
            let wanted = self
                .inner
                .waiters
                .borrow()
                .get(&key)
                .is_some_and(|list| list.iter().any(|waiter| (waiter.wanted)()));
            if wanted {
                break Some(key);
            }
            self.inner.waiters.borrow_mut().remove(&key);
        };
        // The skipped frame jobs go back where they were, so they are still the next frames made.
        let mut queue = self.inner.queue.borrow_mut();
        for entry in skipped_frames.into_iter().rev() {
            queue.push_back(entry);
        }
        found
    }

    /// Hands what was made to the requests that still want it. A job that could not be made is remembered, so
    /// it is not tried again, and its requests are told there is nothing.
    fn finish(&self, key: Key, made: Option<Vec<Pixels>>, stopped: bool) {
        self.inner.running.set(self.inner.running.get() - 1);
        self.inner.cancels.borrow_mut().remove(&key);
        if key.job == Job::Frames {
            self.inner.running_frames.set(self.inner.running_frames.get() - 1);
        }
        if stopped {
            // A job that was stopped made nothing, which is no failure. If something still wants it, it waits its turn again.
            let urgent = {
                let waiters = self.inner.waiters.borrow();
                waiters.get(&key).filter(|list| list.iter().any(|waiter| (waiter.wanted)())).map(|list| list.iter().any(|waiter| waiter.urgent))
            };
            match urgent {
                Some(urgent) => self.enqueue(key, urgent),
                None => {
                    self.inner.waiters.borrow_mut().remove(&key);
                }
            }
            self.start_jobs();
            return;
        }
        let waiters = self.inner.waiters.borrow_mut().remove(&key).unwrap_or_default();
        let textures: Vec<gdk::Texture> = match made {
            Some(made) => {
                let textures: Vec<gdk::Texture> = made.iter().map(Pixels::texture).collect();
                self.remember(key.clone(), textures.clone());
                textures
            }
            None => {
                self.inner.failed.borrow_mut().insert(key.clone());
                Vec::new()
            }
        };
        let mut held = false;
        for waiter in waiters {
            if (waiter.wanted)() {
                self.hold(&key, waiter.wanted);
                held = true;
                (waiter.on_ready)(&textures);
            }
        }
        // Made for items that have left the screen since, so nothing will say when to let it go.
        if !held && !textures.is_empty() {
            self.unload_soon();
        }
        self.start_jobs();
    }

    /// Keeps what was made in memory, and evicts the least recently used things while over the budget.
    fn remember(&self, key: Key, textures: Vec<gdk::Texture>) {
        let mut bytes = self.inner.bytes.get() + bytes_of(&textures);
        if let Some(replaced) = self.inner.textures.borrow_mut().insert(key.clone(), textures) {
            bytes -= bytes_of(&replaced);
        }
        self.touch(&key);
        let mut cached = self.inner.textures.borrow_mut();
        let mut used = self.inner.used.borrow_mut();
        let mut stamps = self.inner.stamps.borrow_mut();
        while bytes > MAX_BYTES && cached.len() > 1 {
            let Some((old, stamp)) = used.pop_front() else { break };
            if stamps.get(&old) == Some(&stamp) {
                stamps.remove(&old);
                if let Some(removed) = cached.remove(&old) {
                    bytes -= bytes_of(&removed);
                }
            }
        }
        self.inner.bytes.set(bytes);
    }
}

/// The number of threads on the system, the most that thumbnails are made on.
pub fn thread_limit() -> usize {
    std::thread::available_parallelism().map_or(1, |count| count.get())
}

/// The memory the pixels of `textures` take up, at four bytes a pixel.
fn bytes_of(textures: &[gdk::Texture]) -> usize {
    textures
        .iter()
        .map(|texture| {
            texture.intrinsic_width().max(0) as usize * texture.intrinsic_height().max(0) as usize * 4
        })
        .sum()
}

fn key(path: &Path, mtime: i64, size: Size, job: Job) -> Key {
    Key {
        path: path.to_path_buf(),
        mtime,
        size,
        job,
    }
}

/// The decoded pixels of one thumbnail. Decoding happens on a worker thread; only the cheap wrap into a texture
/// happens on the main loop, since a texture cannot leave the main thread.
pub(crate) struct Pixels {
    bytes: glib::Bytes,
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
}

impl Pixels {
    /// The pixels of the picture at `path`, decoded in full.
    pub(crate) fn from_file(path: &Path) -> Result<Self, glib::Error> {
        gdk_pixbuf::Pixbuf::from_file(path).map(|pixbuf| Self::from_pixbuf(&pixbuf))
    }

    fn from_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> Self {
        Self {
            bytes: pixbuf.read_pixel_bytes(),
            width: pixbuf.width(),
            height: pixbuf.height(),
            stride: pixbuf.rowstride() as usize,
            alpha: pixbuf.has_alpha(),
        }
    }

    /// The pixels of a PNG, decoded.
    fn from_png(data: &[u8]) -> Option<Self> {
        let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
        gdk_pixbuf::Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).ok().map(|pixbuf| Self::from_pixbuf(&pixbuf))
    }

    pub(crate) fn texture(&self) -> gdk::Texture {
        let format = if self.alpha { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
        gdk::MemoryTexture::new(self.width, self.height, format, &self.bytes, self.stride).upcast()
    }
}

/// Makes one job on a worker thread, then shows it on the main loop. `cancel` stops it: a program it runs is killed, and
/// what was made, if anything, is let go.
async fn run(thumbnails: Thumbnails, key: Key, cancel: Arc<AtomicBool>) {
    let root = thumbnails.inner.disk_cache.get().then(|| thumbnails.inner.root.clone());
    let work = key.clone();
    let flag = cancel.clone();
    let made = match thumbnails.inner.pool.as_ref().map(|pool| pool.push_future(move || make_cancellable(root.as_deref(), &work, flag))) {
        Some(Ok(made)) => made.await.ok().flatten(),
        Some(Err(error)) => {
            eprintln!("rsimgv: cannot start a thumbnail thread: {error}");
            None
        }
        None => None,
    };
    let stopped = cancel.load(Ordering::Relaxed);
    thumbnails.finish(key, if stopped { None } else { made }, stopped);
}

thread_local! {
    /// The flag that stops the job the thread is making, for the programs it runs to look at.
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// `make`, with `cancel` set where the programs it runs can see it.
fn make_cancellable(root: Option<&Path>, key: &Key, cancel: Arc<AtomicBool>) -> Option<Vec<Pixels>> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    CANCEL.with(|current| *current.borrow_mut() = Some(cancel));
    let made = make(root, key);
    CANCEL.with(|current| *current.borrow_mut() = None);
    made
}

/// The pixels of the largest still thumbnail in the cache under `root` for the file at `path` modified at `mtime`, if there
/// is one larger than `above` pixels. A thumbnail made for another time is not current and is passed over.
fn largest_on_disk(root: &Path, path: &Path, mtime: i64, above: i32) -> Option<Pixels> {
    let uri = glib::filename_to_uri(path, None).ok()?;
    let digest = string_hash(uri.as_str());
    SIZES.iter().rev().filter(|size| **size > above).find_map(|size| {
        let data = std::fs::read(filed(&root.join(size.to_string()), &format!("{digest}.png"))).ok()?;
        (png_text(&data, "rsimgv::MTime")?.parse::<i64>().ok()? == mtime).then(|| Pixels::from_png(&data)).flatten()
    })
}

/// The PNG bytes, and the size in pixels, of the largest cached still thumbnail of the file at `path`, as it is now, that is
/// at most `max_size` pixels across. Nothing is made, so `None` for a file that has not been thumbnailed to that size or
/// less, or whose thumbnails are not kept on the disk.
pub fn cached_still_png(path: &Path, max_size: i32) -> Option<(i32, Vec<u8>)> {
    let uri = glib::filename_to_uri(path, None).ok()?;
    let digest = string_hash(uri.as_str());
    let stamp = stamp(path).to_string();
    let root = glib::user_cache_dir().join("rsimgv");
    let found: Vec<(i32, Vec<u8>)> = SIZES
        .iter()
        .filter_map(|&pixels| {
            let data = std::fs::read(filed(&root.join(pixels.to_string()), &format!("{digest}.png"))).ok()?;
            (png_text(&data, "rsimgv::MTime") == Some(stamp.as_str())).then_some((pixels, data))
        })
        .collect();
    found.into_iter().rev().find(|(pixels, _)| *pixels <= max_size)
}

/// The pixels of a job: from the cache under `root` where they are current, or made and cached now. With no `root`,
/// they are made without touching the disk. `None` if the file cannot be decoded.
fn make(root: Option<&Path>, key: &Key) -> Option<Vec<Pixels>> {
    let uri = glib::filename_to_uri(&key.path, None).ok()?;
    let digest = string_hash(uri.as_str());
    match key.job {
        Job::Still(media) => {
            let file = root.map(|root| filed(&root.join(key.size.dir_name()), &format!("{digest}.png")));
            let still = cached_or_made(file.as_deref(), &uri, key.mtime, || still_pixbuf(&key.path, key.size, media))?;
            Some(vec![still])
        }
        Job::Frames => {
            let duration = duration_of(root, &digest, &key.path, key.mtime)?;
            // Frames are spread evenly through the video, at most a second apart, and at most MAX_FRAMES.
            let count = (duration as usize).clamp(1, MAX_FRAMES);
            // A frame that cannot be made is left out, and the others still play.
            let frames: Vec<Pixels> = (0..count)
                .filter_map(|index| {
                    let file = root.map(|root| filed(&root.join("frames").join(key.size.dir_name()), &format!("{digest}-{index}.png")));
                    let at = duration * (index as f64 + 0.5) / count as f64;
                    cached_or_made(file.as_deref(), &uri, key.mtime, || frame_pixbuf(&key.path, at, key.size))
                })
                .collect();
            (!frames.is_empty()).then_some(frames)
        }
    }
}

/// The still thumbnail's picture: the picture scaled down, or a frame a tenth of the way into a video.
fn still_pixbuf(path: &Path, size: Size, media: Media) -> Option<gdk_pixbuf::Pixbuf> {
    match media {
        Media::Picture => {
            let pixels = size.pixels();
            gdk_pixbuf::Pixbuf::from_file_at_scale(path, pixels, pixels, true).ok()
        }
        Media::Video => {
            let at = video_duration(path).map_or(0.0, |duration| duration * 0.1);
            frame_pixbuf(path, at, size)
        }
    }
}

/// The frame of the video at `path` at `seconds`, scaled to fit the size class.
fn frame_pixbuf(path: &Path, seconds: f64, size: Size) -> Option<gdk_pixbuf::Pixbuf> {
    let pixels = size.pixels();
    let png = run_tool(
        Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{seconds:.3}"), "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={pixels}:{pixels}:force_original_aspect_ratio=decrease"),
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-",
            ]),
    )?;
    if png.is_empty() {
        return None;
    }
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(png));
    gdk_pixbuf::Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).ok()
}

/// The length of the video at `path` in seconds. It is read from the record kept for the video under `root` while the
/// file has the modification time `mtime`, so a video whose frames are cached is not probed again. Otherwise `ffprobe`
/// is asked, and the answer is recorded. With no `root`, `ffprobe` is always asked.
fn duration_of(root: Option<&Path>, digest: &str, path: &Path, mtime: i64) -> Option<f64> {
    let Some(root) = root else { return video_duration(path) };
    let record = filed(&root.join("durations"), digest);
    if let Ok(text) = std::fs::read_to_string(&record) {
        let mut lines = text.lines();
        if lines.next().and_then(|line| line.parse::<i64>().ok()) == Some(mtime) {
            if let Some(seconds) = lines.next().and_then(|line| line.parse::<f64>().ok()) {
                return Some(seconds);
            }
        }
    }
    let seconds = video_duration(path)?;
    // A record that cannot be written only costs the next run its probe.
    let _ = write_atomically(&record, format!("{mtime}\n{seconds}\n").as_bytes());
    Some(seconds)
}

/// The length of the video at `path` in seconds, if `ffprobe` knows it.
fn video_duration(path: &Path) -> Option<f64> {
    let output = run_tool(Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(path))?;
    String::from_utf8_lossy(&output)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|seconds| *seconds > 0.0)
}

/// The standard output of `command` if it succeeds within `TOOL_TIMEOUT`.
fn run_tool(command: &mut Command) -> Option<Vec<u8>> {
    run_program(command).ok().flatten()
}

/// What `command` wrote to its standard output, or `None` if it reported failure. An error is returned when it cannot
/// be run, or takes longer than `TOOL_TIMEOUT`: such a run is killed, so a stalled file does not hold its job slot for good.
pub(crate) fn run_program(command: &mut Command) -> std::io::Result<Option<Vec<u8>>> {
    run_within(command, TOOL_TIMEOUT)
}

fn run_within(command: &mut Command, timeout: Duration) -> std::io::Result<Option<Vec<u8>>> {
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let mut stdout = child.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
    // The output is read while the program runs, so a large frame does not fill the pipe and stall it.
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        stdout.read_to_end(&mut data).map(|_| data)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        // A job that was stopped does not wait for its program, which is killed.
        if CANCEL.with(|current| current.borrow().as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed))) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let data = reader.join().map_err(|_| std::io::Error::other("the output reader panicked"))??;
    Ok(status.success().then_some(data))
}

/// A stamp of when the file at `path` was last modified: its time in nanoseconds since the epoch, or 0 if it cannot
/// be read. Thumbnails are kept for the stamp they were made at, so a rewrite within the same second is not missed.
pub fn stamp(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as i64)
}

/// The pixels at `file` if they were made from the file at `mtime`. Otherwise the pixbuf from `produce`, saved
/// as a thumbnail with its metadata, and cached at `file`. With no `file`, just the pixbuf from `produce`.
fn cached_or_made(file: Option<&Path>, uri: &str, mtime: i64, produce: impl FnOnce() -> Option<gdk_pixbuf::Pixbuf>) -> Option<Pixels> {
    let Some(file) = file else { return produce().map(|picture| Pixels::from_pixbuf(&picture)) };
    // A cached thumbnail is current only if the file has not changed since it was made. Its time is read from
    // the PNG's text chunks, so the picture is decoded once, when it is shown.
    if let Ok(data) = std::fs::read(file) {
        if png_text(&data, "rsimgv::MTime").and_then(|value| value.parse::<i64>().ok()) == Some(mtime) {
            if let Some(pixels) = Pixels::from_png(&data) {
                return Some(pixels);
            }
        }
    }

    let picture = produce()?;
    let mtime_text = mtime.to_string();
    let data = picture
        .save_to_bufferv(
            "png",
            &[
                ("tEXt::Thumb::URI", uri),
                ("tEXt::rsimgv::MTime", mtime_text.as_str()),
                ("tEXt::Thumb::Software", "rsimgv"),
            ],
        )
        .ok()?;
    // A cache that cannot be written only costs the next run its decode.
    let _ = write_atomically(file, &data);
    Some(Pixels::from_pixbuf(&picture))
}

/// The text of the `keyword` tEXt chunk in the PNG `data`, read from the chunk headers without decoding the
/// picture. Text chunks come before the picture data, so the search stops there.
fn png_text<'a>(data: &'a [u8], keyword: &str) -> Option<&'a str> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut rest = data.strip_prefix(SIGNATURE)?;
    while rest.len() >= 12 {
        let length = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = &rest[4..8];
        let body = rest.get(8..8 + length)?;
        match kind {
            b"tEXt" => {
                let mut parts = body.splitn(2, |&byte| byte == 0);
                if parts.next()? == keyword.as_bytes() {
                    return std::str::from_utf8(parts.next()?).ok();
                }
            }
            b"IDAT" | b"IEND" => return None,
            _ => {}
        }
        rest = rest.get(12 + length..)?;
    }
    None
}

/// The path of the cache file `name` in `folder`, filed under the first two characters of its name, so that no folder
/// holds very many files: `folder/<first>/<second>/<name>`. Names start with a hex digest, so they are ASCII.
fn filed(folder: &Path, name: &str) -> PathBuf {
    folder.join(&name[..1]).join(&name[1..2]).join(name)
}

/// Writes `data` to `file` through a temporary file, so a reader never sees half of it.
pub(super) fn write_atomically(file: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = file.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    crate::storage::atomic_file::write(file, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test: its cache directory, and the path of a file in it.
    /// The one file under `dir`, at any depth.
    fn only_file(dir: &Path) -> PathBuf {
        let mut files = Vec::new();
        let mut folders = vec![dir.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        assert_eq!(files.len(), 1, "one file under {dir:?}");
        files.pop().unwrap()
    }

    fn folder(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rsimgv-thumbs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("cache"), dir)
    }

    fn picture(dir: &Path, width: i32, height: i32) -> PathBuf {
        let path = dir.join("picture.png");
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, width, height).unwrap();
        pixbuf.fill(0x3366ccff);
        pixbuf.savev(&path, "png", &[]).unwrap();
        path
    }

    /// A short test video, or `None` when `ffmpeg` is not installed.
    fn video(dir: &Path) -> Option<PathBuf> {
        let path = dir.join("clip.mkv");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=4:size=320x240:rate=10", "-c:v", "mpeg4"])
            .arg(&path)
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    #[test]
    fn makes_a_thumbnail_once_and_reuses_it_while_the_picture_is_unchanged() {
        let (root, dir) = folder("reuse");
        let path = picture(&dir, 600, 300);
        let key = |mtime| key(&path, mtime, Size::for_pixels(128), Job::Still(Media::Picture));
        let first = make(Some(&root), &key(1000)).expect("made");
        let file = only_file(&root.join("128"));
        let made = gdk_pixbuf::Pixbuf::from_file(&file).unwrap();
        assert!(made.width() <= 128 && made.height() <= 128);
        assert_eq!(made.option("tEXt::rsimgv::MTime").as_deref(), Some("1000"));
        assert!(made.option("tEXt::Thumb::URI").unwrap().starts_with("file://"));

        // Same modification time: the cached file is used, so it is not rewritten.
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let again = make(Some(&root), &key(1000)).expect("read");
        assert_eq!(first[0].bytes.as_ref(), again[0].bytes.as_ref());
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified);

        // A changed picture makes a new thumbnail.
        make(Some(&root), &key(2000)).expect("remade");
        let remade = gdk_pixbuf::Pixbuf::from_file(&file).unwrap();
        assert_eq!(remade.option("tEXt::rsimgv::MTime").as_deref(), Some("2000"));
    }

    #[test]
    fn without_the_cache_a_thumbnail_is_made_without_touching_the_disk() {
        let (root, dir) = folder("no-cache");
        let path = picture(&dir, 600, 300);
        let key = key(&path, 1000, Size::for_pixels(128), Job::Still(Media::Picture));
        let made = make(None, &key).expect("made");
        assert!(made[0].width <= 128 && made[0].height <= 128);
        assert!(!root.exists(), "nothing is written");

        // What the cache holds is not read either: a thumbnail cached for the same time is passed over.
        make(Some(&root), &key).expect("cached");
        let file = only_file(&root.join("128"));
        std::fs::write(&file, b"not a thumbnail").unwrap();
        assert!(make(None, &key).is_some());
        assert_eq!(std::fs::read(&file).unwrap(), b"not a thumbnail", "nothing is rewritten");
    }

    #[test]
    fn a_cached_thumbnail_can_be_requested_again_from_its_callback() {
        // The cache is not borrowed while a callback runs, so a callback may ask for another thumbnail.
        let (root, dir) = folder("reentrant");
        let path = picture(&dir, 16, 16);
        let key = key(&path, 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let thumbnails = Thumbnails::with_root(root);
        thumbnails.remember(key.clone(), vec![Pixels::from_file(&path).expect("read").texture()]);
        let again = thumbnails.clone();
        let seen = Rc::new(Cell::new(0));
        let counted = seen.clone();
        thumbnails.request(key.clone(), None, || true, move |_| {
            again.request(key.clone(), None, || true, move |textures| counted.set(textures.len()));
        });
        assert_eq!(seen.get(), 1);
    }

    #[test]
    fn the_newest_wanted_job_goes_first_and_unwanted_ones_are_dropped() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        let job = |name: &str| key(Path::new(name), 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let waiter = |wanted: bool| Waiter {
            wanted: Rc::new(move || wanted),
            urgent: true,
            rank: None,
            on_ready: Box::new(|_| {}),
        };
        let (old, scrolled_away, newest) = (job("old.png"), job("away.png"), job("new.png"));
        {
            let mut waiters = thumbnails.inner.waiters.borrow_mut();
            waiters.insert(old.clone(), vec![waiter(true)]);
            waiters.insert(scrolled_away.clone(), vec![waiter(false)]);
            waiters.insert(newest.clone(), vec![waiter(true)]);
        }
        for job in [old.clone(), scrolled_away.clone(), newest.clone()] {
            thumbnails.enqueue(job, true);
        }

        assert!(thumbnails.next_wanted() == Some(newest));
        assert!(thumbnails.next_wanted() == Some(old));
        assert!(thumbnails.next_wanted().is_none());
        assert!(!thumbnails.inner.waiters.borrow().contains_key(&scrolled_away));
    }

    #[test]
    fn jobs_for_things_on_screen_go_before_the_rest_whatever_the_order() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        let job = |name: &str| key(Path::new(name), 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let waiter = |urgent: bool| Waiter { wanted: Rc::new(|| true), urgent, rank: None, on_ready: Box::new(|_| {}) };
        let (visible, near, far) = (job("visible.png"), job("near.png"), job("far.png"));
        {
            let mut waiters = thumbnails.inner.waiters.borrow_mut();
            waiters.insert(visible.clone(), vec![waiter(true)]);
            let ranked = |distance: f64| Waiter { wanted: Rc::new(|| true), urgent: false, rank: Some(Rc::new(move || distance)), on_ready: Box::new(|_| {}) };
            waiters.insert(near.clone(), vec![ranked(1.0)]);
            waiters.insert(far.clone(), vec![ranked(9.0)]);
        }
        // The far one is asked for first, then the near one, then the one on screen, so they come out near-first after it.
        thumbnails.enqueue(far.clone(), false);
        thumbnails.enqueue(near.clone(), false);
        thumbnails.enqueue(visible.clone(), true);
        assert!(thumbnails.next_wanted() == Some(visible));
        assert!(thumbnails.next_wanted() == Some(near.clone()), "the nearest of the rest goes next, whatever the order asked");
        // Asked for again from the screen, the far one jumps ahead of anything left waiting.
        thumbnails.enqueue(job("later.png"), false);
        thumbnails.enqueue(far.clone(), true);
        assert!(thumbnails.next_wanted() == Some(far));
    }

    #[test]
    fn the_levels_reach_further_and_wake_up_for_playback() {
        let pages: Vec<Option<f64>> = Aggression::ALL.iter().map(|level| level.pages()).collect();
        assert_eq!(pages, [Some(0.0), Some(1.0), Some(2.0), Some(4.0), None]);
        assert!(!Aggression::Standard.works_during_playback() && Aggression::Increased.works_during_playback() && Aggression::Extreme.works_during_playback());
        assert_eq!(Aggression::from_name("high"), Aggression::High);
        assert_eq!(Aggression::from_name("nonsense"), Aggression::Standard);
        assert!(Aggression::ALL.iter().all(|level| Aggression::from_name(level.name()) == *level));
    }

    #[test]
    fn asking_again_drops_the_requests_that_no_longer_want_it() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        let job = key(Path::new("picture.png"), 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let waiter = |wanted: bool| Waiter {
            wanted: Rc::new(move || wanted),
            urgent: true,
            rank: None,
            on_ready: Box::new(|_| {}),
        };
        thumbnails.inner.waiters.borrow_mut().insert(job.clone(), vec![waiter(false), waiter(true), waiter(false)]);

        thumbnails.request(job.clone(), None, || true, |_| {});

        assert_eq!(thumbnails.inner.waiters.borrow()[&job].len(), 2);
    }

    #[test]
    fn a_program_that_takes_too_long_is_stopped() {
        let outcome = run_within(Command::new("sleep").arg("30"), Duration::from_millis(50));
        assert_eq!(outcome.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_program_is_told_apart_from_its_failure() {
        let output = run_program(Command::new("sh").args(["-c", "printf done"])).unwrap();
        assert_eq!(output.as_deref(), Some(&b"done"[..]));
        assert_eq!(run_program(&mut Command::new("false")).unwrap(), None);
        assert!(run_program(&mut Command::new("rsimgv-no-such-program")).is_err());
    }

    #[test]
    fn the_thread_count_is_held_to_what_the_system_has() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        thumbnails.set_thread_count(0);
        assert_eq!(thumbnails.inner.max_jobs.get(), 1);
        thumbnails.set_thread_count(usize::MAX);
        assert_eq!(thumbnails.inner.max_jobs.get(), thread_limit());
    }

    #[test]
    fn no_more_jobs_run_than_the_thread_count() {
        let (root, dir) = folder("thread-count");
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let thumbnails = Thumbnails::with_root(root);
                thumbnails.set_thread_count(1);
                for index in 0..3 {
                    thumbnails.request(
                        key(&dir.join(format!("missing-{index}.png")), 1, Size::for_pixels(128), Job::Still(Media::Picture)),
                        None,
                        || true,
                        |_| {},
                    );
                    assert_eq!(thumbnails.inner.running.get(), 1);
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                while !thumbnails.inner.waiters.borrow().is_empty() {
                    assert!(Instant::now() < deadline, "the jobs finish");
                    assert!(thumbnails.inner.running.get() <= 1);
                    context.iteration(false);
                }
            })
            .unwrap();
    }

    /// Thumbnails with one file in memory, requested by something on screen while the returned flag says so.
    fn loaded(name: &str) -> (Thumbnails, PathBuf, Rc<Cell<bool>>) {
        let (root, dir) = folder(name);
        let path = picture(&dir, 16, 16);
        let thumbnails = Thumbnails::with_root(root);
        let job = key(&path, 1, Size::for_pixels(128), Job::Still(Media::Picture));
        thumbnails.remember(job.clone(), vec![Pixels::from_file(&path).expect("read").texture()]);
        let on_screen = Rc::new(Cell::new(true));
        let flag = on_screen.clone();
        thumbnails.request(job, None, move || flag.get(), |_| {});
        (thumbnails, path, on_screen)
    }

    #[test]
    fn a_thumbnail_is_let_go_once_no_item_shows_it() {
        let (thumbnails, path, on_screen) = loaded("unload");
        thumbnails.unload_offscreen();
        assert!(thumbnails.has_loaded(&path), "still on screen");

        on_screen.set(false);
        thumbnails.unload_offscreen();
        assert!(!thumbnails.has_loaded(&path), "no longer on screen");
        assert_eq!(thumbnails.inner.bytes.get(), 0);
        assert!(thumbnails.inner.holders.borrow().is_empty());
    }

    #[test]
    fn asking_again_and_again_keeps_only_the_latest_holders() {
        let (thumbnails, path, _on_screen) = loaded("holders");
        let job = key(&path, 1, Size::for_pixels(128), Job::Still(Media::Picture));
        for _ in 0..3 * MAX_HOLDERS {
            thumbnails.request(job.clone(), None, || true, |_| {});
        }
        assert_eq!(thumbnails.inner.holders.borrow()[&job].len(), MAX_HOLDERS);
    }

    #[test]
    fn with_unloading_off_a_thumbnail_stays_in_memory() {
        let (thumbnails, path, on_screen) = loaded("no-unload");
        thumbnails.inner.unload_offscreen.set(false);
        on_screen.set(false);
        thumbnails.unload_offscreen();
        assert!(thumbnails.has_loaded(&path));
        assert!(thumbnails.inner.holders.borrow().is_empty(), "what is off screen is still forgotten");
    }

    #[test]
    fn sizes_are_the_next_one_up_from_the_item() {
        assert_eq!(Size::for_pixels(20).pixels(), 32);
        assert_eq!(Size::for_pixels(96).pixels(), 128);
        assert_eq!(Size::for_pixels(128).pixels(), 128);
        assert_eq!(Size::for_pixels(200).pixels(), 256);
        assert_eq!(Size::for_pixels(1500).pixels(), 2048);
        assert_eq!(Size::for_pixels(9000).pixels(), 2048);
    }

    #[test]
    fn a_file_that_is_not_a_picture_makes_no_thumbnail() {
        let (root, dir) = folder("not-picture");
        let path = dir.join("text.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(make(Some(&root), &key(&path, 1, Size::for_pixels(128), Job::Still(Media::Picture))).is_none());
    }

    #[test]
    fn a_video_gives_a_still_and_a_sequence_of_frames() {
        let (root, dir) = folder("video");
        let Some(path) = video(&dir) else {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        };
        let still = make(Some(&root), &key(&path, 7, Size::for_pixels(128), Job::Still(Media::Video))).expect("still");
        assert_eq!(still.len(), 1);
        let frames = make(Some(&root), &key(&path, 7, Size::for_pixels(128), Job::Frames)).expect("frames");
        // A four-second clip gives one frame a second.
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert!(frame.width <= 128 && frame.height <= 128);
        }
    }

    #[test]
    fn a_cap_is_the_largest_size_at_most_the_setting() {
        assert_eq!(Size::at_most(1).pixels(), 32);
        assert_eq!(Size::at_most(100).pixels(), 64);
        assert_eq!(Size::at_most(256).pixels(), 256);
        assert_eq!(Size::at_most(9000).pixels(), 2048);
        assert_eq!(Size::for_pixels(1500).min(Size::at_most(512)).pixels(), 512, "an item's size is capped");
        assert_eq!(Size::for_pixels(100).min(Size::at_most(512)).pixels(), 128, "a smaller item keeps its size");
    }

    #[test]
    fn the_cache_time_is_read_from_the_png_text_chunks() {
        let (_, dir) = folder("text-chunk");
        let path = picture(&dir, 16, 16);
        let pixbuf = gdk_pixbuf::Pixbuf::from_file(&path).unwrap();
        let data = pixbuf.save_to_bufferv("png", &[("tEXt::rsimgv::MTime", "1234")]).unwrap();
        assert_eq!(png_text(&data, "rsimgv::MTime"), Some("1234"));
        assert_eq!(png_text(&data, "Thumb::Missing"), None);
        assert_eq!(png_text(b"not a png", "rsimgv::MTime"), None);
    }
}
