//! Thumbnails of pictures and videos, kept in the app's own cache (`~/.cache/rsimgv`). A thumbnail is made
//! on a worker thread at the smallest size that covers the item, and written as a PNG. A picture whose
//! thumbnail is current for its modification time is not decoded again.
//!
//! A video's still thumbnail is a frame a tenth of the way in. Hovering a video asks for a sequence of up
//! to `MAX_FRAMES` frames spread through it, cached beside the thumbnails. Frames come from the `ffmpeg`
//! and `ffprobe` programs; without them, videos keep their icon.
//!
//! Like Nautilus's thumbnailer, only a few are made at a time, and a request that no item on screen
//! wants any more is dropped before it runs. They are made on a thread pool of their own, so however many
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

use gtk::glib::object::Cast;
use gtk::prelude::PaintableExt;
use gtk::{gdk, gdk_pixbuf, gio, glib};

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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Size(i32);

impl Size {
    /// The smallest size that covers `pixels`: the next size up, which is then scaled down for the item.
    pub fn for_pixels(pixels: i32) -> Self {
        Self(SIZES.iter().copied().find(|&size| size >= pixels).unwrap_or(SIZES[SIZES.len() - 1]))
    }

    pub fn pixels(self) -> i32 {
        self.0
    }

    fn dir_name(self) -> String {
        self.0.to_string()
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

/// A request for a thumbnail. `wanted` says whether it is still needed.
struct Waiter {
    wanted: Rc<dyn Fn() -> bool>,
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
    queued: RefCell<HashMap<Key, u64>>,
    running: Cell<usize>,
    /// How many jobs may run at the same time.
    max_jobs: Cell<usize>,
    /// The threads that make the jobs.
    pool: Option<glib::ThreadPool>,
    /// How many of the running jobs are frame jobs.
    running_frames: Cell<usize>,
    /// The things that could not be made, so they are not tried again.
    failed: RefCell<HashSet<Key>>,
}

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
        let key = key(path, mtime, size, Job::Still(media));
        self.request(key, wanted, move |textures| on_done(textures.first()));
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
        self.request(key(path, mtime, size, Job::Frames), wanted, on_ready);
    }

    fn request(&self, key: Key, wanted: impl Fn() -> bool + 'static, on_ready: impl FnOnce(&[gdk::Texture]) + 'static) {
        let cached = self.inner.textures.borrow().get(&key).cloned();
        if let Some(textures) = cached {
            self.touch(&key);
            on_ready(&textures);
            return;
        }
        if self.inner.failed.borrow().contains(&key) {
            on_ready(&[]);
            return;
        }
        let waiter = Waiter {
            wanted: Rc::new(wanted),
            on_ready: Box::new(on_ready),
        };
        let mut waiters = self.inner.waiters.borrow_mut();
        if let Some(list) = waiters.get_mut(&key) {
            // Requests that no longer want it are dropped first, so the list does not grow each time a cell asks again.
            list.retain(|waiter| (waiter.wanted)());
            list.push(waiter);
            drop(waiters);
            // Asked for again while queued, so it is wanted now: it moves to the front of the queue.
            if self.inner.queued.borrow().contains_key(&key) {
                self.enqueue(key);
            }
            return;
        }
        waiters.insert(key.clone(), vec![waiter]);
        drop(waiters);
        self.enqueue(key);
        self.start_jobs();
    }

    /// Puts `key` at the newest end of the queue.
    fn enqueue(&self, key: Key) {
        let stamp = self.next_stamp();
        self.inner.queued.borrow_mut().insert(key.clone(), stamp);
        let mut queue = self.inner.queue.borrow_mut();
        queue.push_back((key, stamp));
        // Moved jobs leave stale entries behind; they are cleared out once they outnumber the live ones.
        if queue.len() > 2 * self.inner.queued.borrow().len() + 64 {
            let queued = self.inner.queued.borrow();
            queue.retain(|(key, stamp)| queued.get(key) == Some(stamp));
        }
    }

    fn next_stamp(&self) -> u64 {
        let stamp = self.inner.stamp.get() + 1;
        self.inner.stamp.set(stamp);
        stamp
    }

    /// The newest queued job with its stamp, skipping stale entries.
    fn pop_newest(&self) -> Option<(Key, u64)> {
        loop {
            let (key, stamp) = self.inner.queue.borrow_mut().pop_back()?;
            if self.inner.queued.borrow().get(&key) == Some(&stamp) {
                return Some((key, stamp));
            }
        }
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
        while self.inner.running.get() < self.inner.max_jobs.get() {
            let Some(key) = self.next_wanted() else { return };
            self.inner.running.set(self.inner.running.get() + 1);
            if key.job == Job::Frames {
                self.inner.running_frames.set(self.inner.running_frames.get() + 1);
            }
            glib::spawn_future_local(run(self.clone(), key));
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
    fn finish(&self, key: Key, made: Option<Vec<Pixels>>) {
        self.inner.running.set(self.inner.running.get() - 1);
        if key.job == Job::Frames {
            self.inner.running_frames.set(self.inner.running_frames.get() - 1);
        }
        let waiters = self.inner.waiters.borrow_mut().remove(&key).unwrap_or_default();
        let textures: Vec<gdk::Texture> = match made {
            Some(made) => {
                let textures: Vec<gdk::Texture> = made.iter().map(Pixels::texture).collect();
                self.remember(key, textures.clone());
                textures
            }
            None => {
                self.inner.failed.borrow_mut().insert(key);
                Vec::new()
            }
        };
        for waiter in waiters {
            if (waiter.wanted)() {
                (waiter.on_ready)(&textures);
            }
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

/// Makes one job on a worker thread, then shows it on the main loop.
async fn run(thumbnails: Thumbnails, key: Key) {
    let root = thumbnails.inner.root.clone();
    let work = key.clone();
    let made = match thumbnails.inner.pool.as_ref().map(|pool| pool.push_future(move || make(&root, &work))) {
        Some(Ok(made)) => made.await.ok().flatten(),
        Some(Err(error)) => {
            eprintln!("rsimgv: cannot start a thumbnail thread: {error}");
            None
        }
        None => None,
    };
    thumbnails.finish(key, made);
}

/// The pixels of a job: from the cache under `root` where they are current, or made and cached now.
/// `None` if the file cannot be decoded.
fn make(root: &Path, key: &Key) -> Option<Vec<Pixels>> {
    let uri = glib::filename_to_uri(&key.path, None).ok()?;
    let digest = glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri.as_str())?;
    match key.job {
        Job::Still(media) => {
            let file = root.join(key.size.dir_name()).join(format!("{digest}.png"));
            let still = cached_or_made(&file, &uri, key.mtime, || still_pixbuf(&key.path, key.size, media))?;
            Some(vec![still])
        }
        Job::Frames => {
            let duration = duration_of(root, &digest, &key.path, key.mtime)?;
            // Frames are spread evenly through the video, at most a second apart, and at most MAX_FRAMES.
            let count = (duration as usize).clamp(1, MAX_FRAMES);
            // A frame that cannot be made is left out, and the others still play.
            let frames: Vec<Pixels> = (0..count)
                .filter_map(|index| {
                    let file = root.join("frames").join(key.size.dir_name()).join(format!("{digest}-{index}.png"));
                    let at = duration * (index as f64 + 0.5) / count as f64;
                    cached_or_made(&file, &uri, key.mtime, || frame_pixbuf(&key.path, at, key.size))
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
/// is asked, and the answer is recorded.
fn duration_of(root: &Path, digest: &str, path: &Path, mtime: i64) -> Option<f64> {
    let record = root.join("durations").join(digest);
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
pub(super) fn run_program(command: &mut Command) -> std::io::Result<Option<Vec<u8>>> {
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
/// as a thumbnail with its metadata, and cached at `file`.
fn cached_or_made(file: &Path, uri: &str, mtime: i64, produce: impl FnOnce() -> Option<gdk_pixbuf::Pixbuf>) -> Option<Pixels> {
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

/// Writes `data` to `file` through a temporary file, so a reader never sees half of it.
pub(super) fn write_atomically(file: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = file.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    crate::atomic_file::write(file, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test: its cache directory, and the path of a file in it.
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
        let first = make(&root, &key(1000)).expect("made");
        let file = root.join("128").read_dir().unwrap().next().unwrap().unwrap().path();
        let made = gdk_pixbuf::Pixbuf::from_file(&file).unwrap();
        assert!(made.width() <= 128 && made.height() <= 128);
        assert_eq!(made.option("tEXt::rsimgv::MTime").as_deref(), Some("1000"));
        assert!(made.option("tEXt::Thumb::URI").unwrap().starts_with("file://"));

        // Same modification time: the cached file is used, so it is not rewritten.
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let again = make(&root, &key(1000)).expect("read");
        assert_eq!(first[0].bytes.as_ref(), again[0].bytes.as_ref());
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified);

        // A changed picture makes a new thumbnail.
        make(&root, &key(2000)).expect("remade");
        let remade = gdk_pixbuf::Pixbuf::from_file(&file).unwrap();
        assert_eq!(remade.option("tEXt::rsimgv::MTime").as_deref(), Some("2000"));
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
        thumbnails.request(key.clone(), || true, move |_| {
            again.request(key.clone(), || true, move |textures| counted.set(textures.len()));
        });
        assert_eq!(seen.get(), 1);
    }

    #[test]
    fn the_newest_wanted_job_goes_first_and_unwanted_ones_are_dropped() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        let job = |name: &str| key(Path::new(name), 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let waiter = |wanted: bool| Waiter {
            wanted: Rc::new(move || wanted),
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
            thumbnails.enqueue(job);
        }

        assert!(thumbnails.next_wanted() == Some(newest));
        assert!(thumbnails.next_wanted() == Some(old));
        assert!(thumbnails.next_wanted().is_none());
        assert!(!thumbnails.inner.waiters.borrow().contains_key(&scrolled_away));
    }

    #[test]
    fn asking_again_drops_the_requests_that_no_longer_want_it() {
        let thumbnails = Thumbnails::with_root(PathBuf::new());
        let job = key(Path::new("picture.png"), 1, Size::for_pixels(128), Job::Still(Media::Picture));
        let waiter = |wanted: bool| Waiter {
            wanted: Rc::new(move || wanted),
            on_ready: Box::new(|_| {}),
        };
        thumbnails.inner.waiters.borrow_mut().insert(job.clone(), vec![waiter(false), waiter(true), waiter(false)]);

        thumbnails.request(job.clone(), || true, |_| {});

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
        assert!(make(&root, &key(&path, 1, Size::for_pixels(128), Job::Still(Media::Picture))).is_none());
    }

    #[test]
    fn a_video_gives_a_still_and_a_sequence_of_frames() {
        let (root, dir) = folder("video");
        let Some(path) = video(&dir) else {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        };
        let still = make(&root, &key(&path, 7, Size::for_pixels(128), Job::Still(Media::Video))).expect("still");
        assert_eq!(still.len(), 1);
        let frames = make(&root, &key(&path, 7, Size::for_pixels(128), Job::Frames)).expect("frames");
        // A four-second clip gives one frame a second.
        assert_eq!(frames.len(), 4);
        for frame in &frames {
            assert!(frame.width <= 128 && frame.height <= 128);
        }
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
