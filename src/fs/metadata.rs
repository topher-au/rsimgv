//! Metadata of pictures and videos: their size, duration, frame rate, codecs and bit rate. `ffprobe` reads each file
//! once on a worker thread, and its answer is kept in the app's cache (`~/.cache/rsimgv/metadata`) for as long as the
//! file is unchanged. A picture's size still comes from its header when `ffprobe` is not there.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gtk::{gdk_pixbuf, gio, glib};
use serde_json::{json, Value};

use super::thumbnail::{run_program, write_atomically};

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
    pub audio_channels: Option<u32>,
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
            audio_channels: audio.and_then(|stream| stream["channels"].as_u64()).map(|channels| channels as u32),
            bit_rate: number(&format["bit_rate"]).map(|bits| bits as u64),
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

#[derive(Default)]
struct Inner {
    root: PathBuf,
    known: RefCell<HashMap<PathBuf, Known>>,
    waiters: RefCell<HashMap<Key, Vec<Waiter>>>,
    queue: RefCell<VecDeque<Key>>,
    running: Cell<usize>,
}

thread_local! {
    static SHARED: MediaInfo = MediaInfo::with_root(glib::user_cache_dir().join("rsimgv").join("metadata"));
}

/// The metadata of the files of the app: one per process, like the thumbnails.
#[derive(Clone)]
pub struct MediaInfo {
    inner: Rc<Inner>,
}

impl MediaInfo {
    /// The metadata of this app, in the user's cache.
    pub fn shared() -> Self {
        SHARED.with(Self::clone)
    }

    fn with_root(root: PathBuf) -> Self {
        Self {
            inner: Rc::new(Inner {
                root,
                ..Inner::default()
            }),
        }
    }

    /// The metadata of the file at `path`, if it is known for this modification time and size.
    pub fn get(&self, path: &Path, mtime: i64, size: u64) -> Option<MediaMetadata> {
        let known = self.inner.known.borrow();
        let known = known.get(path)?;
        (known.mtime == mtime && known.size == size).then(|| known.metadata.clone())
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
            let mut queue = self.inner.queue.borrow_mut();
            if let Some(position) = queue.iter().position(|queued| queued == &key) {
                queue.remove(position);
                queue.push_back(key);
            }
            return None;
        }
        waiters.insert(key.clone(), vec![waiter]);
        drop(waiters);
        self.inner.queue.borrow_mut().push_back(key);
        self.start_jobs();
        None
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
    fn finish(&self, key: Key, metadata: MediaMetadata) {
        self.inner.running.set(self.inner.running.get() - 1);
        let (path, mtime, size) = key.clone();
        let mut known = self.inner.known.borrow_mut();
        if known.len() >= MAX_KNOWN {
            known.clear();
        }
        known.insert(path, Known { mtime, size, metadata });
        drop(known);
        let waiters = self.inner.waiters.borrow_mut().remove(&key).unwrap_or_default();
        for waiter in waiters {
            if (waiter.wanted)() {
                (waiter.on_ready)();
            }
        }
        self.start_jobs();
    }
}

/// Reads one file on a worker thread, then keeps the result on the main loop.
async fn run(info: MediaInfo, key: Key) {
    let root = info.inner.root.clone();
    let work = key.clone();
    let metadata = gio::spawn_blocking(move || read(&root, &work))
        .await
        .ok()
        .unwrap_or_default();
    info.finish(key, metadata);
}

/// The metadata of the file in `key`: from the cache under `root` while it is current for the file, or else from
/// `ffprobe`, which is then cached. A picture's size falls back to its header when `ffprobe` gives none.
fn read(root: &Path, key: &Key) -> MediaMetadata {
    let (path, mtime, size) = (&key.0, key.1, key.2);
    let file = cache_file(root, path);
    let probe = match cached_probe(&file, mtime, size) {
        Some(probe) => probe,
        // Without ffprobe, or if it stalls, nothing is cached, so the next run asks again.
        None => match run_ffprobe(path) {
            Ok(probe) => {
                let probe = probe.unwrap_or(Value::Null);
                write_cache(&file, mtime, size, &probe);
                probe
            }
            Err(_) => Value::Null,
        },
    };
    let mut metadata = MediaMetadata::from_probe(&probe);
    if metadata.width.is_none() {
        if let Some((_, width, height)) = gdk_pixbuf::Pixbuf::file_info(path) {
            metadata.width = Some(width as u32);
            metadata.height = Some(height as u32);
        }
    }
    metadata
}

/// The cache file of the probe of `path`, named by its URI's digest.
fn cache_file(root: &Path, path: &Path) -> PathBuf {
    let uri = glib::filename_to_uri(path, None).map(|uri| uri.to_string()).unwrap_or_default();
    let digest = glib::compute_checksum_for_string(glib::ChecksumType::Md5, &uri).unwrap_or_default();
    root.join(format!("{digest}.json"))
}

/// The probe in `file`, if it was made for the file at `mtime` and `size`. A probe of nothing is `Some(Null)`.
fn cached_probe(file: &Path, mtime: i64, size: u64) -> Option<Value> {
    let value: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    (value["mtime"].as_i64()? == mtime && value["size"].as_u64()? == size).then(|| value["probe"].clone())
}

fn write_cache(file: &Path, mtime: i64, size: u64, probe: &Value) {
    let data = json!({ "mtime": mtime, "size": size, "probe": probe }).to_string();
    // A cache that cannot be written only costs the next run its probe.
    let _ = write_atomically(file, data.as_bytes());
}

/// `ffprobe`'s answer for the file at `path`: its JSON, or `None` if it could not read the file. An error is
/// returned when `ffprobe` cannot be run at all, or stalls on the file.
fn run_ffprobe(path: &Path) -> std::io::Result<Option<Value>> {
    let output = run_program(
        Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
            .arg(path),
    )?;
    Ok(output.and_then(|output| serde_json::from_slice(&output).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_s_probe_gives_its_size_length_rate_and_audio() {
        let probe = json!({
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
                 "avg_frame_rate": "30000/1001", "disposition": {"attached_pic": 0}},
                {"codec_type": "audio", "codec_name": "aac", "channels": 2}
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
        assert_eq!(metadata.audio_channels, Some(2));
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
                {"codec_type": "audio", "codec_name": "mp3", "channels": 1}
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
