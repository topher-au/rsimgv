//! Metadata of pictures and videos: their size, duration, frame rate, codecs and bit rate. `ffprobe` reads each file
//! once on a worker thread, and its answer is kept in a SQLite database in the user's data directory for as long as the
//! file is unchanged. A picture's size still comes from its header when `ffprobe` is not there.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gtk::{gdk_pixbuf, gio, glib};
use serde_json::Value;

use super::thumbnail::run_program;
use crate::storage::file_settings::content_hash;
use crate::storage::media_index::MediaIndex;
use crate::storage::metadata_store::{MetadataStore, Stored};

/// Files probed at the same time.
const MAX_JOBS: usize = 3;

/// What a picture or video says about itself, as far as the browse view shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// In seconds.
    pub duration: Option<f64>,
    pub frame_rate: Option<f64>,
    /// The codec of a video, or the format of a picture.
    pub codec: Option<String>,
    pub audio_codec: Option<String>,
    /// In bits per second.
    pub bit_rate: Option<u64>,
}

impl MediaMetadata {
    /// The metadata in `ffprobe`'s JSON output. A `null` probe, from a file it could not read, gives nothing.
    pub fn from_probe(probe: &Value) -> Self {
        let streams = probe["streams"].as_array().map(Vec::as_slice).unwrap_or_default();
        // A cover picture on an audio file is a stream too, but it is not the video.
        let video = streams
            .iter()
            .find(|stream| stream["codec_type"] == "video" && stream["disposition"]["attached_pic"].as_i64() != Some(1));
        let audio = streams.iter().find(|stream| stream["codec_type"] == "audio");
        let format = &probe["format"];
        Self {
            width: video.and_then(|stream| stream["width"].as_u64()).map(|width| width as u32),
            height: video.and_then(|stream| stream["height"].as_u64()).map(|height| height as u32),
            duration: number(&format["duration"])
                .or_else(|| video.and_then(|stream| number(&stream["duration"])))
                .filter(|seconds| *seconds > 0.0),
            frame_rate: video.and_then(|stream| rational(&stream["avg_frame_rate"])),
            codec: video.and_then(|stream| stream["codec_name"].as_str()).map(str::to_owned),
            audio_codec: audio.and_then(|stream| stream["codec_name"].as_str()).map(str::to_owned),
            // The video's own rate, which is what a video is encoded at, and the container's total if it has none.
            bit_rate: video
                .and_then(|stream| number(&stream["bit_rate"]))
                .or_else(|| number(&format["bit_rate"]))
                .map(|bits| bits as u64),
        }
    }
}

/// A frame rate as text, to two decimal places at most, without trailing zeros: `30fps`, `29.97fps`.
pub fn format_fps(fps: f64) -> String {
    let text = format!("{fps:.2}");
    format!("{}fps", text.trim_end_matches('0').trim_end_matches('.'))
}

/// A number ffprobe wrote, which it writes as a string.
fn number(value: &Value) -> Option<f64> {
    value.as_str().and_then(|text| text.parse().ok()).or_else(|| value.as_f64())
}

/// A frame rate such as `30000/1001`, as frames a second. `0/0` means unknown.
fn rational(value: &Value) -> Option<f64> {
    let (numerator, denominator) = value.as_str()?.split_once('/')?;
    let (numerator, denominator): (f64, f64) = (numerator.parse().ok()?, denominator.parse().ok()?);
    (numerator > 0.0 && denominator > 0.0).then(|| numerator / denominator)
}

/// A file's metadata as last read, with the modification time and size it was read at.
#[derive(Clone)]
struct Known {
    mtime: i64,
    size: u64,
    metadata: MediaMetadata,
}

/// How many files' metadata is kept in memory. Past this the memory is cleared, and the files are read again when asked.
const MAX_KNOWN: usize = 20_000;

/// A file to probe: its path, modification time in seconds, and size in bytes.
type Key = (PathBuf, i64, u64);

/// A request for a file's metadata. `wanted` says whether it is still needed.
struct Waiter {
    wanted: Rc<dyn Fn() -> bool>,
    on_ready: Box<dyn FnOnce()>,
}

struct Inner {
    /// The database, opened the first time metadata is needed. `None` if it could not be opened, so metadata is read but
    /// not kept between runs.
    store: OnceCell<Option<Rc<dyn MetadataStore>>>,
    known: RefCell<HashMap<PathBuf, Known>>,
    waiters: RefCell<HashMap<Key, Vec<Waiter>>>,
    queue: RefCell<VecDeque<Key>>,
    running: Cell<usize>,
}

thread_local! {
    static SHARED: MediaInfo = MediaInfo::new();
}

/// The metadata of the files of the app: one per process, like the thumbnails.
#[derive(Clone)]
pub struct MediaInfo {
    inner: Rc<Inner>,
}

impl MediaInfo {
    /// The metadata of this app, in one database in the user's data directory.
    pub fn shared() -> Self {
        SHARED.with(Self::clone)
    }

    fn new() -> Self {
        Self {
            inner: Rc::new(Inner {
                store: OnceCell::new(),
                known: RefCell::default(),
                waiters: RefCell::default(),
                queue: RefCell::default(),
                running: Cell::default(),
            }),
        }
    }

    /// The database metadata is kept in. A database that cannot be opened is reported once, and metadata is then read
    /// without being kept.
    fn store(&self) -> Rc<dyn MetadataStore> {
        self.inner
            .store
            .get_or_init(|| match MediaIndex::open(&MediaIndex::file()) {
                Ok(index) => Some(Rc::new(index) as Rc<dyn MetadataStore>),
                Err(err) => {
                    eprintln!("rsimgv: the metadata database cannot be opened, so metadata is not kept: {err:#}");
                    None
                }
            })
            .clone()
            .unwrap_or_else(|| Rc::new(NoStore))
    }

    /// The metadata of the file at `path`, if it is known for this modification time and size: from memory, or else from
    /// the store. Nothing is read here; a caller that gets nothing queues a read with `request`.
    pub fn get(&self, path: &Path, mtime: i64, size: u64) -> Option<MediaMetadata> {
        {
            let known = self.inner.known.borrow();
            if let Some(known) = known.get(path).filter(|known| known.mtime == mtime && known.size == size) {
                return Some(known.metadata.clone());
            }
        }
        let stored = self.store().lookup(path, mtime, size)?;
        self.remember(path.to_path_buf(), mtime, size, stored.metadata.clone());
        Some(stored.metadata)
    }

    /// Keeps the metadata of a file in memory, clearing what is kept first if it is full.
    fn remember(&self, path: PathBuf, mtime: i64, size: u64, metadata: MediaMetadata) {
        let mut known = self.inner.known.borrow_mut();
        if known.len() >= MAX_KNOWN {
            known.clear();
        }
        known.insert(path, Known { mtime, size, metadata });
    }

    /// The metadata of the file at `path`, if it is known now. Otherwise it is read in the background, and
    /// `on_ready` is called once it is, as long as `wanted` still says it is needed.
    pub fn request(
        &self,
        path: &Path,
        mtime: i64,
        size: u64,
        wanted: impl Fn() -> bool + 'static,
        on_ready: impl FnOnce() + 'static,
    ) -> Option<MediaMetadata> {
        if let Some(metadata) = self.get(path, mtime, size) {
            return Some(metadata);
        }
        let key: Key = (path.to_path_buf(), mtime, size);
        crate::debug_log!("playlist-debug: metadata request {} queued={} running={}", anonymous(path), self.inner.queue.borrow().len(), self.inner.running.get());
        let waiter = Waiter {
            wanted: Rc::new(wanted),
            on_ready: Box::new(on_ready),
        };
        let mut waiters = self.inner.waiters.borrow_mut();
        if let Some(list) = waiters.get_mut(&key) {
            // Already being read: this request is told too when it is. Requests that no longer want the file are
            // dropped first, so the list does not grow each time a cell asks again.
            list.retain(|waiter| (waiter.wanted)());
            list.push(waiter);
            drop(waiters);
            // Asked for again while queued, so it is wanted now: it moves to the front of the queue.
            self.prioritise(path, mtime, size);
            return None;
        }
        waiters.insert(key.clone(), vec![waiter]);
        drop(waiters);
        self.inner.queue.borrow_mut().push_back(key);
        self.start_jobs();
        None
    }

    /// Whether a request for the metadata of the file at `path` is still waiting for it.
    pub fn is_pending(&self, path: &Path, mtime: i64, size: u64) -> bool {
        self.inner.waiters.borrow().contains_key(&(path.to_path_buf(), mtime, size))
    }

    /// Moves the file at `path` to the front of the queue, where the next read is taken from, if it is queued.
    pub fn prioritise(&self, path: &Path, mtime: i64, size: u64) {
        let key: Key = (path.to_path_buf(), mtime, size);
        let mut queue = self.inner.queue.borrow_mut();
        if let Some(position) = queue.iter().position(|queued| queued == &key) {
            queue.remove(position);
            queue.push_back(key);
        }
    }

    /// Starts queued files while there is room for them.
    fn start_jobs(&self) {
        while self.inner.running.get() < MAX_JOBS {
            let Some(key) = self.next_wanted() else { return };
            self.inner.running.set(self.inner.running.get() + 1);
            glib::spawn_future_local(run(self.clone(), key));
        }
    }

    /// The newest queued file that something on screen still wants. A file nothing wants any more is dropped.
    fn next_wanted(&self) -> Option<Key> {
        loop {
            let key = self.inner.queue.borrow_mut().pop_back()?;
            let wanted = self
                .inner
                .waiters
                .borrow()
                .get(&key)
                .is_some_and(|list| list.iter().any(|waiter| (waiter.wanted)()));
            if wanted {
                return Some(key);
            }
            self.inner.waiters.borrow_mut().remove(&key);
        }
    }

    /// Keeps what was read and calls the requests that still want it.
    fn finish(&self, key: Key, stored: Stored, persist: bool) {
        self.inner.running.set(self.inner.running.get() - 1);
        let (path, mtime, size) = key.clone();
        crate::debug_log!("playlist-debug: metadata finish {}", anonymous(&path));
        if persist {
            if let Err(err) = self.store().record(&path, mtime, size, &stored) {
                eprintln!("rsimgv: could not save the metadata of {}: {err}", anonymous(&path));
            }
        }
        self.remember(path, mtime, size, stored.metadata);
        let waiters = self.inner.waiters.borrow_mut().remove(&key).unwrap_or_default();
        for waiter in waiters {
            if (waiter.wanted)() {
                (waiter.on_ready)();
            }
        }
        self.start_jobs();
    }
}

/// A stand-in for a file's name in logs: a hash of the path, with the extension kept.
pub fn anonymous(path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    match path.extension() {
        Some(extension) => format!("{:016x}.{}", hasher.finish(), extension.to_string_lossy()),
        None => format!("{:016x}", hasher.finish()),
    }
}

/// Reads one file on a worker thread, then keeps the result on the main loop and saves it to the store. The caller has
/// already looked the file up in the store and found nothing current, so it is read.
async fn run(info: MediaInfo, key: Key) {
    let keeps_hash = info.store().keeps_hash();
    let work = key.clone();
    let probed = gio::spawn_blocking(move || read(&work, keeps_hash)).await.ok().flatten();
    match probed {
        Some(stored) => info.finish(key, stored, true),
        // Without ffprobe, or if it stalls, nothing is saved, so the next run asks again.
        None => info.finish(key, Stored::default(), false),
    }
}

/// The metadata of the file in `key`, read with `ffprobe`, with its content hash if `keeps_hash`. `None` if `ffprobe`
/// cannot be run.
fn read(key: &Key, keeps_hash: bool) -> Option<Stored> {
    let path = &key.0;
    let probe = run_ffprobe(path).ok()?.unwrap_or(Value::Null);
    let mut metadata = MediaMetadata::from_probe(&probe);
    size_from_header(&mut metadata, path);
    let hash = if keeps_hash { content_hash(path).ok() } else { None };
    let file_type = mime_type(path);
    Some(Stored { metadata, hash, file_type: Some(file_type), probe: Some(probe) })
}

/// The MIME type of the file at `path`. It is guessed from the file name, which is instant and reads nothing. Only when
/// the name does not say what the file is is its start read, to tell from its content.
fn mime_type(path: &Path) -> String {
    let (content_type, uncertain) = gio::content_type_guess(Some(path), None::<&[u8]>);
    let content_type = if uncertain { sniff(path).unwrap_or(content_type) } else { content_type };
    gio::content_type_get_mime_type(&content_type).map_or_else(|| content_type.to_string(), |mime| mime.to_string())
}

/// The content type of the first bytes of the file at `path`, for a file whose name does not tell what it is.
fn sniff(path: &Path) -> Option<glib::GString> {
    let mut header = [0; 512];
    let count = std::fs::File::open(path).and_then(|mut file| file.read(&mut header)).ok()?;
    Some(gio::content_type_guess(Some(path), Some(&header[..count])).0)
}

/// Fills in a picture's size from its header, for when `ffprobe` gave none.
fn size_from_header(metadata: &mut MediaMetadata, path: &Path) {
    if metadata.width.is_none() {
        if let Some((_, width, height)) = gdk_pixbuf::Pixbuf::file_info(path) {
            metadata.width = Some(width as u32);
            metadata.height = Some(height as u32);
        }
    }
}

/// A store for when the database cannot be opened: it keeps nothing, so metadata is read again when it is needed.
struct NoStore;

impl MetadataStore for NoStore {
    fn lookup(&self, _path: &Path, _mtime: i64, _size: u64) -> Option<Stored> {
        None
    }

    fn record(&self, _path: &Path, _mtime: i64, _size: u64, _stored: &Stored) -> std::io::Result<()> {
        Ok(())
    }

    fn forget(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn keeps_hash(&self) -> bool {
        false
    }
}

/// The tags `ffprobe` is asked for, on each stream: where a video was taken and when it was made. Tag names are matched
/// exactly, so each spelling a file might use is listed. The tags of pictures are not in `ffprobe`'s output, so they
/// are read elsewhere.
const PROBE_TAGS: &str = "location,location-eng,com.apple.quicktime.location.ISO6709,creation_time,date";

/// `ffprobe`'s answer for the file at `path`: its JSON, or `None` if it could not read the file. An error is
/// returned when `ffprobe` cannot be run at all, or stalls on the file.
fn run_ffprobe(path: &Path) -> std::io::Result<Option<Value>> {
    let entries = format!(
        "format=duration,bit_rate:\
         stream=codec_type,codec_name,width,height,avg_frame_rate,duration,bit_rate:stream_tags={PROBE_TAGS}:\
         stream_disposition=attached_pic"
    );
    let output = run_program(
        Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_entries"])
            .arg(entries)
            .arg(path),
    )?;
    Ok(output.and_then(|output| serde_json::from_slice(&output).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_video_s_probe_gives_its_size_length_rate_and_audio() {
        let probe = json!({
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
                 "avg_frame_rate": "30000/1001", "disposition": {"attached_pic": 0}},
                {"codec_type": "audio", "codec_name": "aac"}
            ],
            "format": {"duration": "12.500000", "bit_rate": "2500000"}
        });
        let metadata = MediaMetadata::from_probe(&probe);
        assert_eq!(metadata.width, Some(1920));
        assert_eq!(metadata.height, Some(1080));
        assert_eq!(metadata.duration, Some(12.5));
        assert!((metadata.frame_rate.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(metadata.codec.as_deref(), Some("h264"));
        assert_eq!(metadata.audio_codec.as_deref(), Some("aac"));
        assert_eq!(metadata.bit_rate, Some(2_500_000));
    }

    #[test]
    fn a_picture_has_a_format_and_no_length_or_rate() {
        let probe = json!({
            "streams": [{"codec_type": "video", "codec_name": "png", "width": 640, "height": 480,
                         "avg_frame_rate": "0/0", "disposition": {"attached_pic": 0}}],
            "format": {"duration": "N/A"}
        });
        let metadata = MediaMetadata::from_probe(&probe);
        assert_eq!((metadata.width, metadata.height), (Some(640), Some(480)));
        assert_eq!(metadata.codec.as_deref(), Some("png"));
        assert_eq!(metadata.duration, None);
        assert_eq!(metadata.frame_rate, None);
    }

    #[test]
    fn a_cover_picture_is_not_the_video() {
        let probe = json!({
            "streams": [
                {"codec_type": "video", "codec_name": "mjpeg", "width": 300, "height": 300,
                 "disposition": {"attached_pic": 1}},
                {"codec_type": "audio", "codec_name": "mp3"}
            ],
            "format": {"duration": "180.0"}
        });
        let metadata = MediaMetadata::from_probe(&probe);
        assert_eq!(metadata.codec, None);
        assert_eq!(metadata.width, None);
        assert_eq!(metadata.audio_codec.as_deref(), Some("mp3"));
    }

    #[test]
    fn frame_rates_show_two_decimals_at_most() {
        assert_eq!(format_fps(30.0), "30fps");
        assert_eq!(format_fps(30000.0 / 1001.0), "29.97fps");
        assert_eq!(format_fps(25.5), "25.5fps");
    }

    #[test]
    fn no_probe_gives_nothing() {
        assert_eq!(MediaMetadata::from_probe(&Value::Null), MediaMetadata::default());
    }
}
