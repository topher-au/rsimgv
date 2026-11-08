//! The main browse view: a grid of the folder's files and folders.
//!
//! Each item is a visual part (the icon, later a thumbnail) in a box of exactly `item-size` pixels,
//! with the name below it (ellipsized, or wrapped to two lines with `wrap-names`) and an info line
//! below that, built from the chosen `InfoField`s. Activating a folder asks to open it; activating a
//! file asks to open it in the viewer.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::fs::thumbnail::{Media as ThumbnailMedia, Size as ThumbnailSize, Thumbnails};
use crate::storage::video_crops::VideoCrops;
use crate::storage::watch_history::{format_duration, WatchHistory};
use super::drag;
use super::thumbnail_paint::{picture_paintable, ScatteredThumbnail, MAX_PHOTOS, PHOTO_SHARE};
use super::video_preview::VideoPreview;
use crate::fs::metadata::{MediaInfo, MediaMetadata};
use crate::fs::{natural_cmp, FileEntry, FileStore, Reading};

/// A file or folder in a browse view, as the IPC reports it.
pub struct ListedFile {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

pub const DEFAULT_ITEM_SIZE: i32 = 128;
/// How long the pointer rests on an item before its description shows.
const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(600);
/// How long the folder must stay unchanged after a change in it before the view lists it again.
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
/// How long the pointer must stay still after keyboard browsing before it hides and the selected item's description shows.
const KEYBOARD_IDLE_DELAY: std::time::Duration = std::time::Duration::from_millis(800);
/// How long each frame of a hovered video shows.
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(350);
/// How long the active item must stay a video before its embedded preview starts, so sweeping the pointer across videos
/// does not load each of them.
const EMBEDDED_START_DELAY: std::time::Duration = std::time::Duration::from_millis(120);
/// How long after the embedded preview is chosen the preview player is made, so the first video does not wait for it.
const EMBEDDED_WARMUP_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// Pieces of information shown under an item's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoField {
    Size,
    Modified,
    ContentType,
    Resolution,
    Duration,
    Framerate,
    Codec,
    Bitrate,
    Audio,
    Views,
    Watched,
}

impl InfoField {
    /// Every field, in the order the preferences list them.
    pub const ALL: [InfoField; 11] = [
        InfoField::Size,
        InfoField::Modified,
        InfoField::ContentType,
        InfoField::Resolution,
        InfoField::Duration,
        InfoField::Framerate,
        InfoField::Codec,
        InfoField::Bitrate,
        InfoField::Audio,
        InfoField::Views,
        InfoField::Watched,
    ];

    /// Whether the field is read from a picture's or video's metadata, rather than the file's own information.
    pub fn needs_metadata(self) -> bool {
        matches!(
            self,
            InfoField::Resolution | InfoField::Duration | InfoField::Framerate | InfoField::Codec | InfoField::Bitrate | InfoField::Audio
        )
    }

    /// The name shown for a field in the preferences.
    pub fn title(self) -> &'static str {
        match self {
            InfoField::Size => "Size",
            InfoField::Modified => "Modified",
            InfoField::ContentType => "Type",
            InfoField::Resolution => "Resolution",
            InfoField::Duration => "Duration",
            InfoField::Framerate => "Frame rate",
            InfoField::Codec => "Codec",
            InfoField::Bitrate => "Bit rate",
            InfoField::Audio => "Audio",
            InfoField::Views => "Views",
            InfoField::Watched => "Watched",
        }
    }

    /// The name a field is saved under in the settings.
    pub fn key(self) -> &'static str {
        match self {
            InfoField::Size => "size",
            InfoField::Modified => "modified",
            InfoField::ContentType => "content-type",
            InfoField::Resolution => "resolution",
            InfoField::Duration => "duration",
            InfoField::Framerate => "framerate",
            InfoField::Codec => "codec",
            InfoField::Bitrate => "bit-rate",
            InfoField::Audio => "audio",
            InfoField::Views => "views",
            InfoField::Watched => "watched",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.key() == key)
    }

    /// Whether the field is one of the viewing-history fields, which inline layouts start on a new line.
    fn is_watch(self) -> bool {
        matches!(self, InfoField::Views | InfoField::Watched)
    }

    /// The field's text for `entry`, if it has one. The metadata fields need `metadata`, once it is read.
    fn text(self, entry: &FileEntry, metadata: Option<&MediaMetadata>) -> Option<String> {
        let info = entry.info();
        match self {
            InfoField::Size if entry.is_dir() => None,
            InfoField::Size => Some(glib::format_size(info.size() as u64).to_string()),
            InfoField::Modified => info
                .modification_date_time()
                .and_then(|time| time.format("%Y-%m-%d %H:%M").ok())
                .map(|text| text.to_string()),
            InfoField::ContentType => info
                .content_type()
                .map(|kind| gio::content_type_get_description(&kind).to_string()),
            InfoField::Resolution => {
                let metadata = metadata?;
                Some(format!("{} × {}", metadata.width?, metadata.height?))
            }
            InfoField::Duration => {
                let seconds = metadata?.duration?;
                Some(super::media_controls::format_time((seconds * 1_000_000.0) as i64))
            }
            // Only a video has a frame rate worth showing; a picture's would be meaningless.
            InfoField::Framerate if thumbnail_media(entry) != Some(ThumbnailMedia::Video) => None,
            InfoField::Framerate => metadata?.frame_rate.map(crate::fs::metadata::format_fps),
            InfoField::Codec => metadata?.codec.clone(),
            InfoField::Bitrate => metadata?.bit_rate.map(|bits| {
                if bits >= 1_000_000 {
                    format!("{:.1} Mb/s", bits as f64 / 1_000_000.0)
                } else {
                    format!("{} kb/s", bits / 1000)
                }
            }),
            InfoField::Audio => metadata?.audio_codec.clone(),
            InfoField::Views => {
                let (views, _) = watch_totals(entry.path())?;
                (views > 0).then(|| format!("{views} view{}", if views == 1 { "" } else { "s" }))
            }
            InfoField::Watched => {
                let (_, seconds) = watch_totals(entry.path())?;
                (seconds >= 0.5).then(|| format!("{} watched", format_duration(seconds)))
            }
        }
    }
}

/// The views and watched time recorded for the file at `path`, looked up by the path the browse view lists it at. The
/// history is keyed by content hash, but reading each file in a folder to find its hash would be far too slow here.
fn watch_totals(path: &Path) -> Option<(u64, f64)> {
    WatchHistory::shared()?.totals_at(path).ok().flatten()
}

/// The info for `entry`: its chosen fields that have a value, joined by `separator`. In inline layouts,
/// `break_before_watch` starts the viewing-history fields on a line of their own; stacked layouts already put each field
/// on its own line, so the break is left to `separator`.
fn info_text(entry: &FileEntry, fields: &[InfoField], separator: &str, break_before_watch: bool, metadata: Option<&MediaMetadata>) -> String {
    let values: Vec<(InfoField, String)> = fields.iter().filter_map(|field| field.text(entry, metadata).map(|text| (*field, text))).collect();
    let break_at = break_before_watch.then(|| values.iter().position(|(field, _)| field.is_watch())).flatten();
    let mut text = String::new();
    for (index, (_, value)) in values.iter().enumerate() {
        if index > 0 {
            if Some(index) == break_at {
                text.push('\n');
            } else {
                text.push_str(separator);
            }
        }
        text.push_str(value);
    }
    text
}

/// The tooltip for an item: its name, kind and path, then each of its details in a grid. The metadata details need
/// `metadata`, once it is read.
fn tooltip_content(entry: &FileEntry, metadata: Option<&MediaMetadata>) -> gtk::Box {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    body.add_css_class("item-tooltip");

    let title = gtk::Label::new(Some(&entry.name()));
    title.add_css_class("tooltip-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    title.set_max_width_chars(40);
    body.append(&title);

    if let Some(kind) = InfoField::ContentType.text(entry, None) {
        let kind_label = gtk::Label::new(Some(&kind));
        kind_label.add_css_class("dim-label");
        kind_label.set_xalign(0.0);
        body.append(&kind_label);
    }
    let path = gtk::Label::new(Some(&entry.path().to_string_lossy()));
    path.add_css_class("tooltip-path");
    path.set_xalign(0.0);
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path.set_max_width_chars(40);
    body.append(&path);

    let details: Vec<(&str, String)> = InfoField::ALL
        .into_iter()
        .filter(|field| *field != InfoField::ContentType)
        .filter_map(|field| field.text(entry, metadata).map(|text| (field.title(), text)))
        .collect();
    if details.is_empty() {
        return body;
    }
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.set_margin_top(6);
    separator.set_margin_bottom(6);
    body.append(&separator);
    let grid = gtk::Grid::new();
    grid.add_css_class("tooltip-grid");
    grid.set_column_spacing(14);
    grid.set_row_spacing(3);
    for (row, (title, text)) in details.iter().enumerate() {
        let key = gtk::Label::new(Some(title));
        key.add_css_class("dim-label");
        key.set_xalign(1.0);
        key.set_valign(gtk::Align::Start);
        let value = gtk::Label::new(Some(text));
        value.set_xalign(0.0);
        value.set_wrap(true);
        value.set_max_width_chars(36);
        grid.attach(&key, 0, row as i32, 1, 1);
        grid.attach(&value, 1, row as i32, 1, 1);
    }
    body.append(&grid);
    body
}

/// The thumbnail `entry` gets: a picture or a video's still. SVG pictures keep their icon.
fn thumbnail_media(entry: &FileEntry) -> Option<ThumbnailMedia> {
    if entry.is_dir() {
        return None;
    }
    let kind = entry.info().content_type()?;
    if kind.starts_with("image/") && !kind.starts_with("image/svg") {
        Some(ThumbnailMedia::Picture)
    } else if kind.starts_with("video/") {
        Some(ThumbnailMedia::Video)
    } else {
        None
    }
}

/// When `entry` was last modified, in seconds since the epoch.
fn modified_secs(entry: &FileEntry) -> i64 {
    entry.info().modification_date_time().map_or(0, |time| time.to_unix())
}

/// The item a cell shows, if it shows one.
/// The runs of neighbouring positions in `descending` (which is in descending order), as (first position, count),
/// in the same order. Removing the runs in that order leaves the positions of the others unchanged.
fn runs(descending: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &position in descending {
        match runs.last_mut() {
            Some((first, count)) if position + 1 == *first => {
                *first = position;
                *count += 1;
            }
            _ => runs.push((position, 1)),
        }
    }
    runs
}

fn entry_in(view: &BrowseView, cell: &gtk::Box) -> Option<FileEntry> {
    let position = cell.widget_name().parse::<u32>().ok()?;
    view.imp().grid.model()?.item(position).and_downcast::<FileEntry>()
}

/// Whether `cell` currently shows the item at `path`.
fn shows(view: &BrowseView, cell: &gtk::Box, path: &Path) -> bool {
    entry_in(view, cell).is_some_and(|entry| entry.path() == path)
}

/// Whether a request for the item at `path` is still wanted: its cell still shows it.
/// Whether a request for the item at `path` is still wanted: its cell still shows it, from the same bind. A cell
/// rebound to the same item, with a new size or layout, is a new bind, so an older request for it is no longer wanted.
fn wanted_for(view: &BrowseView, cell: &gtk::Box, path: &Path) -> impl Fn() -> bool + 'static {
    let generation = bind_generation(view, cell);
    let (view, cell, path) = (view.downgrade(), cell.downgrade(), path.to_path_buf());
    move || match (view.upgrade(), cell.upgrade()) {
        (Some(view), Some(cell)) => shows(&view, &cell, &path) && bind_generation(&view, &cell) == generation,
        _ => false,
    }
}

/// How many times the cell at its position has been bound. The cell's widget name is its position.
fn bind_generation(view: &BrowseView, cell: &gtk::Box) -> u64 {
    let Ok(position) = cell.widget_name().parse::<u32>() else { return 0 };
    view.imp().bind_generations.borrow().get(&position).copied().unwrap_or_default()
}

/// Whether a thumbnail requested at item size `size` for the item at `path` is still wanted: as `wanted_for`, the items are
/// still that size, and the item is near enough to the ones in view for the thumbnail generator's aggressiveness. A thumbnail made for an earlier size would otherwise be shown over the new one.
fn wanted_at(view: &BrowseView, cell: &gtk::Box, path: &Path, size: i32) -> impl Fn() -> bool + 'static {
    let (view_weak, cell_weak, wanted) = (view.downgrade(), cell.downgrade(), wanted_for(view, cell, path));
    move || {
        wanted()
            && view_weak.upgrade().is_some_and(|view| {
                // As far from the items in view as the thumbnail generator's aggressiveness reaches.
                let near = cell_weak.upgrade().and_then(|cell| cell.widget_name().parse::<u32>().ok()).is_some_and(|position| view.thumbnail_wanted(position));
                view.item_size() == size && near
            })
    }
}

/// The image in a cell's visual part.
fn icon_of(cell: &gtk::Box) -> Option<gtk::Image> {
    cell.first_child().and_then(|visual| visual.first_child()).and_downcast::<gtk::Image>()
}

/// The picture in a cell's visual part, where an embedded video preview is shown.
fn player_of(cell: &gtk::Box) -> Option<gtk::Picture> {
    cell.first_child().and_then(|visual| visual.last_child()).and_downcast::<gtk::Picture>()
}

/// How a video previews while it is the active item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum VideoPreviews {
    #[default]
    None,
    /// A sequence of frames captured ahead of time and played in turn.
    Frames,
    /// The video itself, in a player embedded in the item.
    Embedded,
}

impl VideoPreviews {
    /// The mode a setting value names, or `None` for any other.
    fn from_name(name: &str) -> Self {
        match name {
            "frames" => Self::Frames,
            "embedded" => Self::Embedded,
            _ => Self::None,
        }
    }
}

/// Shows the still thumbnail of `entry` in `icon` once it is made, as long as the cell still shows `entry`.
fn request_thumbnail(
    view: &BrowseView,
    cell: &gtk::Box,
    entry: &FileEntry,
    icon: &gtk::Image,
    size: i32,
    media: ThumbnailMedia,
) {
    let path = entry.path().to_path_buf();
    let icon = icon.downgrade();
    // A video's still is drawn between film strips.
    let film = media == ThumbnailMedia::Video;
    let crop = view.thumbnail_crop();
    Thumbnails::shared().request_still(
        &path,
        crate::fs::thumbnail::stamp(&path),
        media,
        ThumbnailSize::for_pixels(size * cell.scale_factor()),
        wanted_at(view, cell, &path, size),
        move |texture| {
            if let Some(icon) = icon.upgrade() {
                icon.set_pixel_size(size);
                icon.set_paintable(Some(&picture_paintable(texture, crop, film)));
                if let Some(visual) = icon.parent() {
                    visual.add_css_class("has-thumbnail");
                }
            }
        },
    );
}

/// How long after the items or the view change the thumbnails to make ahead are worked out, and how many are asked for at a time.
const PREFETCH_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
const PREFETCH_BATCH: usize = 200;

/// The most entries of a folder looked at to find its pictures, so a huge folder does not hold up the view.
const SCATTER_SCAN: usize = 500;

/// The most entries of a folder read to choose those from, by name. A folder with more takes its names from the first
/// ones it lists, so its previews may depend on the order it lists them in.
const SCATTER_READ: usize = 20_000;

/// The most folders whose scans are kept, so the cache stays small when browsing many folders. The least recently
/// used goes first.
const MAX_CACHED_SCANS: usize = 256;

/// Folders scanned at the same time. The scans share GIO's few worker threads with thumbnails and folder listings, so
/// a screen of large folders must not take them all.
const MAX_SCANS: usize = 2;

/// The entries of a folder to look at: the first `SCATTER_SCAN` by name, so the same ones are taken whatever order the
/// folder lists them in, of at most the first `SCATTER_READ` listed.
fn first_by_name(listed: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = listed.take(SCATTER_READ).collect();
    paths.sort();
    paths.truncate(SCATTER_SCAN);
    paths
}

/// Every picture and video of `folder`, with their modification times and kinds, in a shuffled order. The order is
/// random, but the same for the same folder, so scrolling does not reshuffle them. Hidden entries and folders are
/// skipped. This reads the disk, so it runs on a worker thread.
fn scattered_media(folder: &Path) -> Vec<Picture> {
    let mut random = Seeded::for_folder(folder);
    let Ok(read) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut media: Vec<Picture> = first_by_name(read.filter_map(|entry| entry.ok().map(|entry| entry.path())))
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with('.') {
                return None;
            }
            let metadata = path.metadata().ok()?;
            if metadata.is_dir() {
                return None;
            }
            let modified = crate::fs::thumbnail::stamp(&path);
            let size = metadata.len();
            let kind = gio::content_type_guess(Some(&path), None::<&[u8]>).0;
            let media = if kind.starts_with("image/") && !kind.starts_with("image/svg") {
                ThumbnailMedia::Picture
            } else if kind.starts_with("video/") {
                ThumbnailMedia::Video
            } else {
                return None;
            };
            Some((path, modified, size, media))
        })
        .collect();
    // A shuffle, then the first few.
    for index in (1..media.len()).rev() {
        media.swap(index, (random.next() % (index as u64 + 1)) as usize);
    }
    media
}

/// A folder to scan: its path and its modification time.
type ScanKey = (PathBuf, i64);

/// A request for a folder's scan. `wanted` says whether it is still needed.
struct ScanWaiter {
    wanted: Box<dyn Fn() -> bool>,
    on_ready: Box<dyn FnOnce(&Rc<Vec<Picture>>)>,
}

/// A folder's scan, kept with the folder's modification time then and when the scan was last used.
struct KeptScan {
    modified: i64,
    media: Rc<Vec<Picture>>,
    used: u64,
}

/// The folder scans of a view: those kept, and those asked for. Like thumbnails, only a few are made at a time, the
/// newest first, and a scan that no item on screen wants any more is dropped before it runs.
#[derive(Default)]
struct FolderScans {
    kept: HashMap<PathBuf, KeptScan>,
    /// The last use handed out.
    used: u64,
    /// Requests waiting for each scan, whether it is queued or being made.
    waiters: HashMap<ScanKey, Vec<ScanWaiter>>,
    /// Scans waiting to be made, newest last.
    queue: VecDeque<ScanKey>,
    running: usize,
}

impl FolderScans {
    /// The scan kept for `folder`, if the folder has not changed since it was made, which `modified` tells.
    fn get(&mut self, folder: &Path, modified: i64) -> Option<Rc<Vec<Picture>>> {
        self.used += 1;
        let used = self.used;
        let kept = self.kept.get_mut(folder).filter(|kept| kept.modified == modified)?;
        kept.used = used;
        Some(kept.media.clone())
    }

    /// Keeps the scan of `folder`, and lets the least recently used one go if there are too many.
    fn keep(&mut self, folder: PathBuf, modified: i64, media: Rc<Vec<Picture>>) {
        if self.kept.len() >= MAX_CACHED_SCANS && !self.kept.contains_key(&folder) {
            let oldest = self.kept.iter().min_by_key(|(_, kept)| kept.used).map(|(folder, _)| folder.clone());
            if let Some(oldest) = oldest {
                self.kept.remove(&oldest);
            }
        }
        self.used += 1;
        self.kept.insert(folder, KeptScan { modified, media, used: self.used });
    }

    /// Adds a request for the scan `key`. A scan already asked for is not asked for twice; if it is still queued, it
    /// moves to the newest end.
    fn ask(&mut self, key: ScanKey, waiter: ScanWaiter) {
        if let Some(list) = self.waiters.get_mut(&key) {
            // Requests that no longer want it are dropped first, so the list does not grow each time a cell asks again.
            list.retain(|waiter| (waiter.wanted)());
            list.push(waiter);
            if let Some(position) = self.queue.iter().position(|queued| queued == &key) {
                self.queue.remove(position);
                self.queue.push_back(key);
            }
            return;
        }
        self.waiters.insert(key.clone(), vec![waiter]);
        self.queue.push_back(key);
    }

    /// The newest queued scan that something on screen still wants, if there is room to start it. A scan nothing wants
    /// any more is dropped.
    fn start_next(&mut self) -> Option<ScanKey> {
        if self.running >= MAX_SCANS {
            return None;
        }
        while let Some(key) = self.queue.pop_back() {
            if self.waiters.get(&key).is_some_and(|list| list.iter().any(|waiter| (waiter.wanted)())) {
                self.running += 1;
                return Some(key);
            }
            self.waiters.remove(&key);
        }
        None
    }

    /// Keeps the finished scan `key`, and gives back the requests that were waiting for it.
    fn finish(&mut self, key: ScanKey, media: Rc<Vec<Picture>>) -> Vec<ScanWaiter> {
        self.running -= 1;
        let waiters = self.waiters.remove(&key).unwrap_or_default();
        self.keep(key.0, key.1, media);
        waiters
    }
}

/// Starts the queued folder scans while there is room for them. Each runs on a worker thread, and calls the requests
/// that still want it on the main thread.
fn start_scans(view: &BrowseView) {
    loop {
        let Some(key) = view.imp().folder_scans.borrow_mut().start_next() else { return };
        let view = view.downgrade();
        glib::spawn_future_local(async move {
            let scan = key.0.clone();
            let media = Rc::new(gio::spawn_blocking(move || scattered_media(&scan)).await.unwrap_or_default());
            let Some(view) = view.upgrade() else { return };
            let waiters = view.imp().folder_scans.borrow_mut().finish(key, media.clone());
            for waiter in waiters {
                if (waiter.wanted)() {
                    (waiter.on_ready)(&media);
                }
            }
            start_scans(&view);
        });
    }
}

/// How long each set of a folder's preview pictures stays up while it cycles.
const CYCLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// A picture or video of a folder: its path, modification time in seconds, size in bytes, and kind.
type Picture = (PathBuf, i64, u64, ThumbnailMedia);

/// A folder whose preview pictures are cycling through its media.
struct Cycle {
    cell: glib::WeakRef<gtk::Box>,
    folder: PathBuf,
    /// Every picture and video of the folder, in the order the previews take them.
    media: Rc<Vec<Picture>>,
    scattered: ScatteredThumbnail,
    /// The most pictures a page takes.
    slots: usize,
    /// Whether a page takes as many pictures as fit their dimensions, rather than up to `slots`.
    fitted: bool,
    size: i32,
    /// The index in `media` of the first picture shown.
    offset: usize,
    /// The page after the one shown, loading.
    preload: Option<Rc<RefCell<Preload>>>,
}

/// A page of a cycling folder's pictures, loading.
struct Preload {
    /// The index in `media` of the page's first picture.
    offset: usize,
    /// How many pictures the page takes.
    len: usize,
    /// The page's pictures, by slot, with how far each is turned.
    page: Vec<Option<(gtk::gdk::Texture, f32)>>,
    /// The pictures not yet made.
    remaining: usize,
}

/// The offset of the page after one of `len` pictures from `offset`, or the start again after the last page.
fn following(offset: usize, len: usize, total: usize) -> usize {
    let next = offset + len;
    if next < total { next } else { 0 }
}

/// The share of the item's square that a fitted page's pictures may cover.
const FIT_BUDGET: f32 = 0.42;

/// How many pictures a fitted page takes, from their aspect ratios in order: as many as fit in `FIT_BUDGET`. Each
/// picture's longest side is `PHOTO_SHARE` of the item, so a wide or tall picture covers less than a square one.
fn fit_count(aspects: &[f32]) -> usize {
    let mut used = 0.0;
    let mut count = 0;
    for &aspect in aspects {
        let aspect = aspect.max(f32::EPSILON);
        let area = PHOTO_SHARE * PHOTO_SHARE * aspect.min(1.0 / aspect);
        if count > 0 && used + area > FIT_BUDGET {
            break;
        }
        used += area;
        count += 1;
    }
    count.max(1)
}

/// How many of `media` a fitted page takes, from the first.
fn fitted_length(media: &[Picture], aspect: impl FnMut(&Picture) -> f32) -> usize {
    let aspects: Vec<f32> = media.iter().take(MAX_PHOTOS).map(aspect).collect();
    fit_count(&aspects).min(media.len())
}

/// The width over the height of a picture with `metadata`. One not known counts as square.
fn aspect_ratio(metadata: Option<MediaMetadata>) -> f32 {
    match metadata.and_then(|metadata| Some((metadata.width?, metadata.height?))) {
        Some((width, height)) if width > 0 && height > 0 => width as f32 / height as f32,
        _ => 1.0,
    }
}

/// The scattered previews a cell shows, if it shows a folder with its pictures up.
fn scattered_in(cell: &gtk::Box) -> Option<ScatteredThumbnail> {
    icon_of(cell)?.paintable()?.downcast::<ScatteredThumbnail>().ok()
}

/// How far a scattered picture is turned, in degrees: between about ten degrees either way, decided by its file name,
/// so a picture is turned the same way wherever it shows.
fn photo_turn(path: &Path) -> f32 {
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&path.file_name().unwrap_or_default(), &mut hasher);
    Seeded(std::hash::Hasher::finish(&hasher)).signed_unit() * 10.0
}

/// A stream of pseudo-random numbers. It starts from a seed, so the same seed gives the same numbers.
struct Seeded(u64);

impl Seeded {
    /// The stream for `folder`: the same for the same folder path.
    fn for_folder(folder: &Path) -> Self {
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(folder, &mut hasher);
        Self(std::hash::Hasher::finish(&hasher))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A number from -1 to 1.
    fn signed_unit(&mut self) -> f32 {
        ((self.next() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// The folder's icon looked up at `size` pixels. The icon the image shows was made for a smaller size, so drawing it
/// at the item's size would scale it up.
fn folder_paintable(icon: &gtk::Image, entry: &FileEntry, size: i32) -> Option<gtk::gdk::Paintable> {
    let gicon = entry.info().icon()?;
    let theme = gtk::IconTheme::for_display(&icon.display());
    let paintable = theme.lookup_by_gicon(&gicon, size, icon.scale_factor(), gtk::TextDirection::None, gtk::IconLookupFlags::empty());
    Some(paintable.upcast())
}

/// Shows the folder `entry`'s icon filling its item, with up to the preview count of its pictures and videos scattered
/// over it, each once it is made. A folder with none keeps its plain icon. The folder is scanned on a worker thread
/// unless its scan is already kept, and the previews are laid out once the scan is in.
fn request_scattered(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry, icon: &gtk::Image, size: i32) {
    let modified = modified_secs(entry);
    if let Some(media) = view.cached_scan(entry.path(), modified) {
        show_scattered(view, cell, entry, icon, size, &media);
        return;
    }
    let (weak_view, weak_cell, weak_icon) = (view.downgrade(), cell.downgrade(), icon.downgrade());
    let wanted = wanted_at(view, cell, entry.path(), size);
    let key = (entry.path().to_path_buf(), modified);
    let entry = entry.clone();
    let waiter = ScanWaiter {
        wanted: Box::new(wanted),
        on_ready: Box::new(move |media| {
            if let (Some(view), Some(cell), Some(icon)) = (weak_view.upgrade(), weak_cell.upgrade(), weak_icon.upgrade()) {
                show_scattered(&view, &cell, &entry, &icon, size, media);
            }
        }),
    };
    view.imp().folder_scans.borrow_mut().ask(key, waiter);
    start_scans(view);
}

/// Lays out the scattered previews of folder `entry` over its `icon`, from its scanned `media`, and requests each
/// picture that fits.
fn show_scattered(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry, icon: &gtk::Image, size: i32, media: &[Picture]) {
    if media.is_empty() {
        return;
    }
    // The folder's icon fills the item under the pictures, so it is drawn at the item's size.
    icon.set_pixel_size(size);
    let fitted = view.folder_preview_fit();
    let slots = if fitted { MAX_PHOTOS } else { view.preview_count() };
    let shown = view.page_length(cell, entry.path(), media, 0, fitted, slots);
    let base = folder_paintable(icon, entry, size);
    let has_base = base.is_some();
    let scattered = ScatteredThumbnail::new(base, slots, fitted);
    // The previews go up now, showing the folder's icon, so that metadata coming in before the first picture can lay
    // them out. A folder with no icon keeps what the image shows until a picture is made.
    if has_base {
        icon.set_paintable(Some(&scattered));
    }
    for (index, picture) in media.iter().take(shown).enumerate() {
        request_photo(view, cell, icon, &scattered, index, picture, size);
    }
}

/// The thumbnail size for a folder's pictures in items of `size` pixels. Each is drawn at `PHOTO_SHARE` of the item at
/// most, so a thumbnail of the whole item's size would only take up memory.
fn photo_size(size: i32, scale: i32) -> ThumbnailSize {
    ThumbnailSize::for_pixels((size as f32 * PHOTO_SHARE).ceil() as i32 * scale)
}

/// Requests the picture `index` of a folder's scattered previews, and shows it on `icon` once it is made.
fn request_photo(view: &BrowseView, cell: &gtk::Box, icon: &gtk::Image, scattered: &ScatteredThumbnail, index: usize, picture: &Picture, size: i32) {
    let (path, modified, _, kind) = picture;
    let folder = path.parent().unwrap_or(path);
    let icon = icon.downgrade();
    let scattered = scattered.clone();
    let turn = photo_turn(path);
    Thumbnails::shared().request_still(
        path,
        *modified,
        *kind,
        photo_size(size, cell.scale_factor()),
        wanted_at(view, cell, folder, size),
        move |texture| {
            // A newer layout of the folder may already be up on the icon; this picture belongs to the old one.
            let shown = icon.upgrade().and_then(|icon| icon.paintable()).and_then(|paintable| paintable.downcast::<ScatteredThumbnail>().ok());
            if shown.is_some_and(|shown| shown != scattered) {
                return;
            }
            scattered.set_photo(index, texture, turn);
            if let Some(icon) = icon.upgrade() {
                icon.set_pixel_size(size);
                icon.set_paintable(Some(&scattered));
                if let Some(visual) = icon.parent() {
                    visual.add_css_class("has-thumbnail");
                }
            }
        },
    );
}

/// Lays out the scattered previews of the folder `entry` in `cell` again, once a picture's metadata is known. The
/// folder's scan and its preview object are kept, so only the pictures that now fit are requested, and the others
/// are taken off. Only what is known is read: the first layout asked for every picture's metadata, and each answer
/// lays the folder out once. Asking again here would double the layouts with every picture.
fn relayout_scattered(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry) {
    #[cfg(test)]
    tests::RELAYOUTS.with(|count| count.set(count.get() + 1));
    let Some(scattered) = scattered_in(cell) else { return };
    // A cycling folder's previews show its current page, which the cycle lays out itself.
    if view.imp().cycle.borrow().as_ref().is_some_and(|cycle| cycle.scattered == scattered) {
        return;
    }
    let (Some(icon), Some(media)) = (icon_of(cell), view.cached_scan(entry.path(), modified_secs(entry))) else { return };
    let fitted = view.folder_preview_fit();
    let slots = if fitted { MAX_PHOTOS } else { view.preview_count() };
    let shown = if fitted {
        fitted_length(&media, |(path, modified, size, _)| aspect_ratio(MediaInfo::shared().get(path, *modified, *size)))
    } else {
        slots.min(media.len())
    };
    let size = view.item_size();
    for (index, picture) in media.iter().take(slots).enumerate() {
        if index < shown {
            request_photo(view, cell, &icon, &scattered, index, picture, size);
        } else {
            scattered.clear_photo(index);
        }
    }
}

/// Whether the hover playback `generation` is the current one.
fn is_current_hover(hovered: &Option<Hover>, generation: u64) -> bool {
    hovered.as_ref().is_some_and(|hover| hover.generation == generation)
}

/// Whether the entry's name starts with a dot, which hides it unless hidden files are shown.
fn is_hidden(entry: &FileEntry) -> bool {
    entry.path().file_name().is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
}

/// Whether `entry` is a picture or a video, judged by its content type.
fn is_media(entry: &FileEntry) -> bool {
    entry
        .info()
        .content_type()
        .is_some_and(|kind| kind.starts_with("image/") || kind.starts_with("video/"))
}

/// Whether `entry`'s content type starts with `prefix`, such as `image/`.
fn has_content_type(entry: &FileEntry, prefix: &str) -> bool {
    entry.info().content_type().is_some_and(|kind| kind.starts_with(prefix))
}

/// What the files are sorted by.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SortBy {
    #[default]
    Name,
    Modified,
    Size,
    Type,
}

/// The sort settings, as the sorter reads them.
#[derive(Clone, Copy, Default)]
struct Order {
    by: SortBy,
    descending: bool,
    /// Whether folders are sorted among the files, rather than before them.
    inline_folders: bool,
}

/// The filter settings, as the filter reads them.
#[derive(Clone, Copy, Default)]
struct Visibility {
    show_hidden: bool,
    media_only: bool,
    show_images: bool,
    show_videos: bool,
    /// Whether folders are listed at all.
    folders_listed: bool,
}

/// What a search matches an item's name or attribute against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchField {
    /// The file or folder's name.
    #[default]
    Name,
    /// Its name, or any attribute it has.
    AnyAttribute,
    /// One attribute, shown the way the item's information shows it.
    Attribute(InfoField),
}

impl SearchField {
    /// The fields a search can match, in the order the search field lists them. Only the attributes that do not need a
    /// picture's or video's metadata are listed, since reading that for every item would make a search slow.
    pub const ALL: [SearchField; 5] = [
        SearchField::Name,
        SearchField::AnyAttribute,
        SearchField::Attribute(InfoField::Size),
        SearchField::Attribute(InfoField::Modified),
        SearchField::Attribute(InfoField::ContentType),
    ];

    /// The name shown for the field.
    pub fn title(self) -> &'static str {
        match self {
            SearchField::Name => "Name",
            SearchField::AnyAttribute => "Any Attribute",
            SearchField::Attribute(field) => field.title(),
        }
    }
}

/// The search over the items: the text to look for, what it is looked for in, and whether the subfolders of the folder
/// shown are searched too, rather than only its own items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// The text, in lower case and without its surrounding spaces. Empty when nothing is searched for.
    needle: String,
    field: SearchField,
    recursive: bool,
}

impl Search {
    pub fn new(text: &str, field: SearchField, recursive: bool) -> Self {
        Self { needle: text.trim().to_lowercase(), field, recursive }
    }

    /// Whether there is text to search for. Without it, every item is shown.
    pub fn is_active(&self) -> bool {
        !self.needle.is_empty()
    }

    /// Whether the items are the folders' contents, rather than the folder's own: the subfolders are listed too, and
    /// the items are matched among all of them.
    fn lists_tree(&self) -> bool {
        self.recursive && self.is_active()
    }

    /// Whether `entry` matches the text in the field searched.
    fn matches(&self, entry: &FileEntry) -> bool {
        if !self.is_active() {
            return true;
        }
        let contains = |text: &str| text.to_lowercase().contains(&self.needle);
        match self.field {
            SearchField::Name => contains(entry.sort_name()),
            SearchField::AnyAttribute => {
                contains(entry.sort_name())
                    || InfoField::ALL
                        .into_iter()
                        .filter(|field| !field.needs_metadata())
                        .filter_map(|field| field.text(entry, None))
                        .any(|text| contains(&text))
            }
            SearchField::Attribute(field) => field.text(entry, None).is_some_and(|text| contains(&text)),
        }
    }
}

/// Orders `a` and `b` by the `by` key. Names are equal under `Name`, so the caller breaks those ties.
fn compare_by(by: SortBy, a: &FileEntry, b: &FileEntry) -> std::cmp::Ordering {
    match by {
        SortBy::Modified => a.modified().cmp(&b.modified()),
        SortBy::Size => a.size().cmp(&b.size()),
        SortBy::Type => a.content_type().cmp(&b.content_type()),
        SortBy::Name => std::cmp::Ordering::Equal,
    }
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::BrowseView)]
    #[template(resource = "/au/topher/rsimgv/ui/browse_view.ui")]
    pub struct BrowseView {
        #[template_child]
        pub grid: TemplateChild<super::super::ItemGrid>,
        #[template_child]
        pub floating_bar: TemplateChild<super::super::FloatingBar>,
        /// Whether a folder is loading. Its listing is stopped by the floating bar's stop button.
        pub(super) loading: Cell<bool>,
        /// Whether the saved scroll position is still being restored after a folder lists.
        pub(super) restoring_scroll: Cell<bool>,
        /// What runs once the folder it is for has listed, with that folder.
        pub(super) pending: RefCell<Option<(PathBuf, Box<dyn FnOnce(&super::BrowseView)>)>>,
        /// The sorted model, so its sorter can be set aside while a folder lists.
        pub(super) sorted: OnceCell<gtk::SortListModel>,
        /// How many times each position's cell has been bound, to tell a request made for an earlier bind from a current one.
        pub(super) bind_generations: RefCell<HashMap<u32, u64>>,
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter: OnceCell<gtk::CustomFilter>,
        pub(super) sorter: OnceCell<gtk::CustomSorter>,
        /// The selection the grid shows. It is taken off the grid while a folder loads.
        pub(super) selection: OnceCell<gtk::SingleSelection>,
        /// Show dot-prefixed files and folders.
        #[property(get, set)]
        show_hidden: Cell<bool>,
        /// Show only pictures and videos (and folders), not every file.
        #[property(get, set)]
        media_only: Cell<bool>,
        /// Show pictures. Hiding them leaves every other kind of file as it was.
        #[property(get, set)]
        show_images: Cell<bool>,
        /// Show videos.
        #[property(get, set)]
        show_videos: Cell<bool>,
        /// Crop each picture and video thumbnail to a square that fills the item's visual part.
        #[property(get, set)]
        thumbnail_crop: Cell<bool>,
        /// How a video previews while it is the active item: `none`, `frames` or `embedded`.
        #[property(get, set)]
        video_frame_previews: RefCell<String>,
        /// The mpv command line options videos are played with, which an embedded preview uses too.
        #[property(get, set)]
        mpv_options: RefCell<String>,
        /// Show a cascade of a folder's first pictures and videos over its icon.
        #[property(get, set)]
        directory_previews: Cell<bool>,
        /// Cycle a folder's preview pictures through its media while the folder is the active item.
        #[property(get, set)]
        folder_preview_cycle: Cell<bool>,
        /// Fit as many previews to a folder's icon as their pictures' dimensions allow, rather than a set number.
        #[property(get, set)]
        folder_preview_fit: Cell<bool>,
        /// The most pictures and videos scattered over a folder's icon.
        #[property(get, set)]
        folder_preview_count: Cell<i32>,
        /// The read of the folder shown. Opening another folder, or stopping, cancels it.
        pub(super) reading: RefCell<Option<Reading>>,
        /// Whether a refresh was asked for while the folder was being read, so it is made once the read is done.
        pub(super) refresh_skipped: Cell<bool>,
        /// The video whose frames are playing: the active item, when it is a video.
        pub(super) hovered: RefCell<Option<Hover>>,
        pub(super) hover_generation: Cell<u64>,
        /// The video playing in an embedded player in the active item.
        pub(super) embedded: RefCell<Option<Embedded>>,
        /// The timer that starts the embedded preview once the active item has stayed a moment, and the cell and path it
        /// will show.
        pub(super) embedded_timer: RefCell<Option<glib::SourceId>>,
        pub(super) pending_embedded: RefCell<Option<(glib::WeakRef<gtk::Box>, PathBuf)>>,
        /// The timer that makes the preview player ahead of the first preview.
        pub(super) embedded_warmup: RefCell<Option<glib::SourceId>>,
        /// The folder whose preview pictures are cycling, if any.
        pub(super) cycle: RefCell<Option<Cycle>>,
        pub(super) cycle_timer: RefCell<Option<glib::SourceId>>,
        /// The pictures and videos scanned for each folder's previews.
        pub(super) folder_scans: RefCell<FolderScans>,
        /// The item under the pointer, for the folder preview cycle.
        pub(super) hovered_cell: RefCell<Option<glib::WeakRef<gtk::Box>>>,
        /// The popover that shows an item's description, below the item, and the timer that opens it.
        pub(super) tooltip: OnceCell<gtk::Popover>,
        pub(super) tooltip_timer: RefCell<Option<glib::SourceId>>,
        /// Whether the selection was last moved from the keyboard, and not since moved by the pointer.
        pub(super) keyboard_browsing: Cell<bool>,
        /// Whether the selection was changed more recently than the pointer moved, so the selected item is the active
        /// one even with the pointer resting over another.
        pub(super) follow_selection: Cell<bool>,
        /// The timer that, once the pointer has been still, hides it and shows the selected item's description.
        pub(super) keyboard_timer: RefCell<Option<glib::SourceId>>,
        /// Whether this is the active tab, or the tab whose viewer is open. Only the active view watches its folder.
        pub(super) active: Cell<bool>,
        /// Whether the viewer is open over the browser, so that no item in it is on screen.
        pub(super) covered: Cell<bool>,
        /// Bumped when what thumbnails are made ahead of time for changes, so the older requests are no longer wanted.
        pub(super) prefetch_generation: Cell<u64>,
        /// The positions already asked for in this generation, the timer that asks for more, and those still to ask for.
        pub(super) prefetched: RefCell<std::collections::HashSet<u32>>,
        pub(super) prefetch_timer: RefCell<Option<glib::SourceId>>,
        pub(super) prefetch_todo: RefCell<VecDeque<u32>>,
        /// When each file was last modified, as thumbnails are asked for, so they are not read from the disk each time.
        pub(super) prefetch_stamps: RefCell<HashMap<PathBuf, i64>>,
        /// Watches the folder shown, while the view is active.
        pub(super) monitor: RefCell<Option<gio::FileMonitor>>,
        /// The timer that lists the folder again after its changes have settled.
        pub(super) refresh_timer: RefCell<Option<glib::SourceId>>,
        /// How far each folder visited in this view was scrolled, so returning to one restores it.
        pub(super) scroll_memory: RefCell<std::collections::HashMap<PathBuf, f64>>,
        pub(super) path: RefCell<Option<PathBuf>>,
        pub(super) info_fields: RefCell<Vec<InfoField>>,
        /// How the info is laid out under a name: `disabled` (hidden), `stacked` (one field a line), or
        /// `inline` (fields separated by a bullet).
        #[property(get, set)]
        info_layout: RefCell<String>,
        /// How item names are shown: `off`, `wrapped` (two lines) or `ellipsized` (one line).
        #[property(get, set)]
        filename_display: RefCell<String>,
        /// Where folders are listed: `none`, `first-last` (before the files), or `inline` among them.
        /// Width and height of the visual part of each item, in pixels.
        #[property(get, set)]
        item_size: Cell<i32>,
        #[property(get, set)]
        show_directories: RefCell<String>,
        /// What the files and folders are sorted by: `name`, `modified`, `size` or `type`.
        #[property(get, set)]
        sort_by: RefCell<String>,
        /// The sort and filter settings as plain values, from the properties above; see `cache_settings`.
        pub(super) order: Cell<Order>,
        pub(super) visibility: Cell<Visibility>,
        /// The search the items are filtered by. Set by `set_search`.
        pub(super) search: RefCell<Search>,
        /// `ascending` or `descending`.
        #[property(get, set)]
        sort_order: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowseView {
        const NAME: &'static str = "RsimgvBrowseView";
        type Type = super::BrowseView;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            // The floating bar and the grid are built from the template, so their types must be registered first.
            super::super::FloatingBar::static_type();
            super::super::ItemGrid::static_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BrowseView {
        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("directory-activated")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("file-activated")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("item-context-requested")
                        .param_types([String::static_type(), bool::static_type(), f64::static_type(), f64::static_type()])
                        .build(),
                    Signal::builder("selection-changed")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("open-in-new-tab-requested")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("listing-changed").build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            self.item_size.set(DEFAULT_ITEM_SIZE);
            self.media_only.set(true);
            self.show_images.set(true);
            self.show_videos.set(true);
            self.filename_display.replace("ellipsized".to_owned());
            self.info_layout.replace("inline".to_owned());
            self.show_directories.replace("first-last".to_owned());
            self.sort_by.replace("name".to_owned());
            self.sort_order.replace("ascending".to_owned());
            self.info_fields
                .replace(vec![InfoField::Size, InfoField::Modified]);
            self.video_frame_previews.replace("none".to_owned());

            // The sorter and filter read their settings as plain values, kept up to date as the properties change.
            obj.cache_settings();
            for name in ["show-hidden", "media-only", "show-images", "show-videos", "show-directories", "sort-by", "sort-order"] {
                obj.connect_notify_local(Some(name), |view, _| view.cache_settings());
            }

            let store = gio::ListStore::new::<FileEntry>();
            // The filter reads the view's settings, so it always reflects the current ones.
            let view = obj.downgrade();
            let filter = gtk::CustomFilter::new(move |item| {
                let Some(entry) = item.downcast_ref::<FileEntry>() else {
                    return true;
                };
                let Some(view) = view.upgrade() else {
                    return true;
                };
                let visibility = view.imp().visibility.get();
                let visible_when_hidden = visibility.show_hidden || !is_hidden(entry);
                let visible_when_media = !visibility.media_only || entry.is_dir() || is_media(entry);
                let visible_when_listed = !entry.is_dir() || visibility.folders_listed;
                let visible_when_kind = entry.is_dir()
                    || !((!visibility.show_images && has_content_type(entry, "image/"))
                        || (!visibility.show_videos && has_content_type(entry, "video/")));
                let matched = view.imp().search.borrow().matches(entry);
                visible_when_hidden && visible_when_media && visible_when_kind && visible_when_listed && matched
            });

            // Folders go before the files, or among them when show-directories is inline.
            let sorter = gtk::CustomSorter::new({
                let view = obj.downgrade();
                move |a, b| {
                    let (Some(a), Some(b), Some(view)) =
                        (a.downcast_ref::<FileEntry>(), b.downcast_ref::<FileEntry>(), view.upgrade())
                    else {
                        return gtk::Ordering::Equal;
                    };
                    let settings = view.imp().order.get();
                    let rank = |entry: &FileEntry| match (settings.inline_folders, entry.is_dir()) {
                        (true, _) => 0,
                        (false, is_dir) => u8::from(!is_dir),
                    };
                    let order = compare_by(settings.by, a, b).then_with(|| natural_cmp(a.sort_name(), b.sort_name()));
                    let order = if settings.descending { order.reverse() } else { order };
                    match rank(a).cmp(&rank(b)).then(order) {
                        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
                    }
                }
            });
            let sorted = gtk::SortListModel::new(Some(store.clone()), Some(sorter.clone()));
            let _ = self.sorted.set(sorted.clone());
            let visible = gtk::FilterListModel::new(Some(sorted), Some(filter.clone()));
            let selection = gtk::SingleSelection::new(Some(visible));
            selection.set_autoselect(false);
            self.grid.set_model(Some(&selection));
            // Thumbnails are made ahead of time for the items near the ones in view, so what is near changes with the items
            // and with the view.
            selection.connect_items_changed({
                let view = obj.downgrade();
                move |_, _, _, _| {
                    if let Some(view) = view.upgrade() {
                        view.restart_prefetch();
                    }
                }
            });
            self.grid.connect_view_moved({
                let view = obj.downgrade();
                move |_| {
                    if let Some(view) = view.upgrade() {
                        view.schedule_prefetch();
                    }
                }
            });
            Thumbnails::shared().watch_aggression({
                let view = obj.downgrade();
                move || {
                    if let Some(view) = view.upgrade() {
                        view.restart_prefetch();
                    }
                }
            });
            selection.connect_selection_changed({
                let view = obj.downgrade();
                move |_, _, _| {
                    if let Some(view) = view.upgrade() {
                        view.update_status();
                        view.imp().follow_selection.set(true);
                        // A folder being listed clears its selection; that is not the user's choice, so it is not reported.
                        if !view.imp().loading.get() {
                            view.imp().restoring_scroll.set(false);
                            view.emit_by_name::<()>("selection-changed", &[&view.selected_name().unwrap_or_default()]);
                        }
                        view.update_previews();
                    }
                }
            });
            self.floating_bar.connect_stop({
                let view = obj.downgrade();
                move |_| {
                    if let Some(view) = view.upgrade() {
                        view.stop_loading();
                    }
                }
            });
            if let Some(overlay) = self.floating_bar.parent() {
                self.floating_bar.attach_hover_hide(&overlay);
            }
            let _ = self.selection.set(selection);
            let _ = self.store.set(store);
            let _ = self.filter.set(filter);
            let _ = self.sorter.set(sorter);

            self.grid.set_builders(
                || {
                    let builder = gtk::Builder::from_resource("/au/topher/rsimgv/ui/browse_item.ui");
                    let cell: gtk::Box = builder.object("cell").expect("browse_item.ui defines `cell`");

                // Right-click on an item asks for its context menu.
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                click.connect_pressed(|gesture, n_press, x, y| {
                    if n_press != 1 {
                        return;
                    }
                    let Some(cell) = gesture.widget().and_downcast::<gtk::Box>() else { return };
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    let Ok(position) = cell.widget_name().parse::<u32>() else { return };
                    let Some(entry) = view.imp().grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>() else {
                        return;
                    };
                    let point = cell
                        .compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32))
                        .map_or((x, y), |p| (f64::from(p.x()), f64::from(p.y())));
                    let path = entry.path().to_string_lossy().into_owned();
                    view.emit_by_name::<()>(
                        "item-context-requested",
                        &[&path, &entry.is_dir(), &point.0, &point.1],
                    );
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                });
                cell.add_controller(click);

                // Middle-click on a folder asks for it to be opened in a new tab.
                let middle = gtk::GestureClick::new();
                middle.set_button(gtk::gdk::BUTTON_MIDDLE);
                middle.connect_pressed(|gesture, _, _, _| {
                    let Some(cell) = gesture.widget().and_downcast::<gtk::Box>() else { return };
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    let Ok(position) = cell.widget_name().parse::<u32>() else { return };
                    let Some(entry) = view.imp().grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>() else {
                        return;
                    };
                    if entry.is_dir() {
                        let path = entry.path().to_string_lossy().into_owned();
                        view.emit_by_name::<()>("open-in-new-tab-requested", &[&path]);
                    }
                });
                cell.add_controller(middle);

                // Dragging an item carries it to other applications, with the marked items if it is one of them.
                let drag = gtk::DragSource::new();
                drag.set_actions(gtk::gdk::DragAction::COPY);
                drag.connect_prepare(|source, _, _| drag_content(&source.widget()?));
                cell.add_controller(drag);

                // Hovering a video plays its frames; leaving it shows the still thumbnail again.
                let motion = gtk::EventControllerMotion::new();
                motion.connect_enter(|controller, _, _| {
                    let Some(cell) = controller.widget().and_downcast::<gtk::Box>() else { return };
                    cell.add_css_class("hovered");
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    view.hover_enter(&cell);
                    view.tooltip_enter(&cell);
                });
                motion.connect_leave(|controller| {
                    let Some(cell) = controller.widget().and_downcast::<gtk::Box>() else { return };
                    cell.remove_css_class("hovered");
                    let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() else {
                        return;
                    };
                    view.hover_leave(&cell);
                    view.tooltip_leave();
                });
                cell.add_controller(motion);
                    cell.upcast()
                },
                {
                    let view = obj.downgrade();
                    move |widget, entry, position| {
                        let (Some(cell), Some(view)) = (widget.downcast_ref::<gtk::Box>(), view.upgrade()) else {
                            return;
                        };
                        *view.imp().bind_generations.borrow_mut().entry(position).or_default() += 1;
                        style_item(&view, cell, entry);
                    }
                },
                |widget| {
                    // A spare cell must not keep its item's thumbnail or preview in memory.
                    if let Some(cell) = widget.downcast_ref::<gtk::Box>() {
                        if let Some(view) = cell.ancestor(super::BrowseView::static_type()).and_downcast::<super::BrowseView>() {
                            view.stop_embedded_in(cell);
                        }
                    }
                    let thumbnails = Thumbnails::shared();
                    if thumbnails.unloads_offscreen() {
                        if let Some(icon) = widget.downcast_ref::<gtk::Box>().and_then(icon_of) {
                            icon.clear();
                        }
                    }
                    thumbnails.unload_soon();
                },
            );

            // The description of the item under the pointer, in a popover below the item rather than a
            // tooltip, which GTK places at the pointer and so over the thumbnail.
            let popover = gtk::Popover::new();
            popover.set_autohide(false);
            popover.set_has_arrow(false);
            popover.set_position(gtk::PositionType::Bottom);
            popover.set_can_target(false);
            let _ = self.tooltip.set(popover);

            // Cursor movement is exposed as actions on the `browse` group, so keys and menus can share them.
            let actions = gtk::gio::SimpleActionGroup::new();
            for (name, move_cursor) in [
                ("cursor-x", crate::ui::ItemGrid::move_cursor_x as fn(&crate::ui::ItemGrid, i32)),
                ("cursor-y", crate::ui::ItemGrid::move_cursor_y),
                ("cursor-page", crate::ui::ItemGrid::move_cursor_page),
            ] {
                let action = gtk::gio::SimpleAction::new(name, Some(gtk::glib::VariantTy::INT32));
                let grid = self.grid.downgrade();
                action.connect_activate(move |_, parameter| {
                    let (Some(grid), Some(n)) = (grid.upgrade(), parameter.and_then(|value| value.get::<i32>())) else {
                        return;
                    };
                    move_cursor(&grid, n);
                });
                actions.add_action(&action);
            }
            obj.insert_action_group("browse", Some(&actions));

            let view = obj.downgrade();
            self.grid.connect_activate(move |_, position| {
                if let Some(view) = view.upgrade() {
                    view.activate(position);
                }
            });
            let view = obj.downgrade();
            self.grid.connect_marks_changed(move |_| {
                if let Some(view) = view.upgrade() {
                    view.update_status();
                }
            });

            // A folder dropped on the view is opened, and a file dropped on it shows the folder it is in.
            let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
            drop.connect_drop({
                let view = obj.downgrade();
                move |_, value, _, _| {
                    let Some(view) = view.upgrade() else { return false };
                    let Some(path) = drag::dropped_paths(value).into_iter().next() else { return false };
                    let folder = if path.is_dir() { Some(path.as_path()) } else { path.parent() };
                    let Some(folder) = folder else { return false };
                    if view.path().as_deref() == Some(folder) {
                        return false;
                    }
                    view.emit_by_name::<()>("directory-activated", &[&folder.to_string_lossy().into_owned()]);
                    true
                }
            });
            obj.add_controller(drop);

            // Changing the size or the name mode rebinds every item, which re-lays them out.
            obj.connect_item_size_notify(|view| view.refresh_items());
            obj.connect_filename_display_notify(|view| view.refresh_items());
            obj.connect_info_layout_notify(|view| view.refresh_items());
            obj.connect_show_hidden_notify(|view| {
                view.refresh_filter();
                // The subfolders searched depend on whether hidden ones are shown.
                view.relist_if_tree();
            });
            obj.connect_media_only_notify(|view| view.refresh_filter());
            obj.connect_show_images_notify(|view| view.refresh_filter());
            obj.connect_show_videos_notify(|view| view.refresh_filter());
            obj.connect_thumbnail_crop_notify(|view| view.refresh_items());
            obj.connect_directory_previews_notify(|view| view.refresh_items());
            obj.connect_folder_preview_cycle_notify(|view| view.update_cycle());
            obj.connect_video_frame_previews_notify(|view| {
                view.update_video_preview();
                view.prewarm_embedded_player();
            });
            obj.connect_mpv_options_notify(|view| view.prewarm_embedded_player());
            obj.connect_folder_preview_count_notify(|view| view.refresh_items());
            obj.connect_folder_preview_fit_notify(|view| view.refresh_items());
            obj.connect_show_directories_notify(|view| {
                view.refresh_filter();
                view.resort();
            });
            obj.connect_sort_by_notify(|view| view.resort());
            obj.connect_sort_order_notify(|view| view.resort());

            let pointer = gtk::EventControllerMotion::new();
            pointer.connect_motion({
                let view = obj.downgrade();
                move |_, _, _| {
                    if let Some(view) = view.upgrade() {
                        view.pointer_moved();
                    }
                }
            });
            obj.add_controller(pointer);
        }
    }

    impl WidgetImpl for BrowseView {}
    impl BoxImpl for BrowseView {}
}

/// The video whose frames play. `generation` tells the frame playback whether it is still current.
struct Hover {
    cell: glib::WeakRef<gtk::Box>,
    path: PathBuf,
    generation: u64,
}

/// The video playing in an embedded player in a cell.
struct Embedded {
    cell: glib::WeakRef<gtk::Box>,
    path: PathBuf,
    stream: crate::mpv::MpvStream,
}

/// The item at the top of the view and how far it is above the view's top edge.
#[derive(Clone)]
struct TopItem {
    position: u32,
    offset: f64,
}

/// Finds the rendered item nearest the top edge of the grid's scroller, and where it sits.
fn top_item_anchor(grid: &super::ItemGrid) -> Option<TopItem> {
    let scroller = grid.ancestor(gtk::ScrolledWindow::static_type())?;
    let mut rendered = Vec::new();
    let mut child = grid.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        let Ok(position) = cell.widget_name().parse::<u32>() else { continue };
        let Some(bounds) = cell.compute_bounds(&scroller) else { continue };
        rendered.push((position, f64::from(bounds.y())));
    }
    // Prefer the last item at or above the top edge; otherwise the first one below it.
    let (position, y) = rendered
        .iter()
        .filter(|(_, y)| *y <= 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| rendered.iter().min_by(|a, b| a.1.total_cmp(&b.1)))?;
    Some(TopItem {
        position: *position,
        offset: *y,
    })
}

/// Current position of the anchored item within the grid's scroller, if it is rendered.
fn anchored_item_y(grid: &super::ItemGrid, position: u32) -> Option<f64> {
    let scroller = grid.ancestor(gtk::ScrolledWindow::static_type())?;
    let mut child = grid.first_child();
    while let Some(cell) = child {
        child = cell.next_sibling();
        if cell.widget_name() == position.to_string() {
            return cell.compute_bounds(&scroller).map(|bounds| f64::from(bounds.y()));
        }
    }
    None
}

/// Scrolls the grid so the anchored item is back at its old offset. Layout can take a few frames
/// and the scroll may be clamped along the way, so the correction is re-applied for a while; once
/// the item is where it was, re-applying changes nothing.
fn restore_top_item_after_layout(grid: &super::ItemGrid, anchor: TopItem) {
    const FRAMES: u32 = 8;
    let grid = grid.clone();
    let frames = Cell::new(0u32);
    grid.clone().add_tick_callback(move |_, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        if let (Some(scroller), Some(y)) = (grid.ancestor(gtk::ScrolledWindow::static_type()), anchored_item_y(&grid, anchor.position)) {
            if let Some(scroller) = scroller.downcast_ref::<gtk::ScrolledWindow>() {
                let adjustment = scroller.vadjustment();
                adjustment.set_value(adjustment.value() + (y - anchor.offset));
            }
        }
        if frames.get() < FRAMES {
            return glib::ControlFlow::Continue;
        }
        glib::ControlFlow::Break
    });
}

/// The content of a drag from the item in `cell`: every marked item if it is one of them, else the item alone.
fn drag_content(cell: &gtk::Widget) -> Option<gtk::gdk::ContentProvider> {
    let view = cell.ancestor(BrowseView::static_type()).and_downcast::<BrowseView>()?;
    let position = cell.widget_name().parse::<u32>().ok()?;
    let entry = view.imp().grid.model()?.item(position).and_downcast::<FileEntry>()?;
    let marked = view.imp().grid.marked_paths();
    let paths = if marked.iter().any(|path| path == entry.path()) {
        marked
    } else {
        vec![entry.path().to_path_buf()]
    };
    Some(drag::file_list(&paths))
}

/// Lays out one item: a visual box of exactly the item size, the name, and the info line.
fn style_item(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry) {
    let size = view.item_size();
    let Some(visual) = cell.first_child().and_downcast::<gtk::Overlay>() else { return };
    let Some(name) = visual.next_sibling().and_downcast::<gtk::Label>() else { return };
    let Some(info) = name.next_sibling().and_downcast::<gtk::Label>() else { return };
    let Some(icon) = visual.first_child().and_downcast::<gtk::Image>() else { return };

    visual.set_size_request(size, size);
    cell.set_size_request(size, -1);
    // A folder's icon fills its item, as its previews do; other icons are a little smaller.
    let icon_size = if entry.is_dir() { size } else { (size * 3 / 5).max(16) };
    icon.set_pixel_size(icon_size);
    match entry.info().icon() {
        Some(gicon) => icon.set_from_gicon(&gicon),
        None => icon.set_icon_name(Some("text-x-generic-symbolic")),
    }
    visual.remove_css_class("has-thumbnail");
    // A folder's visual has no background of its own, with or without previews.
    if entry.is_dir() {
        visual.add_css_class("folder");
    } else {
        visual.remove_css_class("folder");
    }
    if let Some(media) = thumbnail_media(entry) {
        request_thumbnail(view, cell, entry, &icon, size, media);
    } else if entry.is_dir() && view.directory_previews() {
        request_scattered(view, cell, entry, &icon, size);
    }

    let display = view.filename_display();
    let wrap = display == "wrapped";
    name.set_label(&entry.name());
    name.set_visible(display != "off");
    name.set_wrap(wrap);
    name.set_lines(if wrap { 2 } else { 1 });
    name.set_ellipsize(if wrap {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::Middle
    });
    // Names and info are capped to about the item's width, so long text does not widen the item.
    let chars = (size / 8).max(4);
    name.set_max_width_chars(chars);
    info.set_max_width_chars(chars);
    // Information too long for the item's width wraps onto more lines rather than being cut off.
    info.set_wrap(true);
    info.set_wrap_mode(gtk::pango::WrapMode::WordChar);

    update_info(view, cell, entry, &info);
}

/// The label under `cell`'s name, which shows the info fields.
fn info_label_of(cell: &gtk::Box) -> Option<gtk::Label> {
    let name = cell.first_child()?.next_sibling()?;
    name.next_sibling().and_downcast::<gtk::Label>()
}

/// Shows the chosen info fields of `entry` in `info`. The metadata fields wait for the metadata to be read, and show
/// once it is.
fn update_info(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry, info: &gtk::Label) {
    let fields = view.imp().info_fields.borrow().clone();
    let layout = view.info_layout();
    let text = if layout == "disabled" {
        String::new()
    } else {
        let separator = if layout == "stacked" { "\n" } else { " · " };
        let break_before_watch = layout != "stacked";
        let metadata = if fields.iter().any(|field| field.needs_metadata()) && thumbnail_media(entry).is_some() {
            request_metadata(view, cell, entry)
        } else {
            None
        };
        info_text(entry, &fields, separator, break_before_watch, metadata.as_ref())
    };
    info.set_label(&text);
    info.set_visible(!text.is_empty());
}

/// The metadata of the picture or video `entry`, if it is read. Otherwise it is read in the background, and the info
/// line of `cell` is shown again once it is.
fn request_metadata(view: &BrowseView, cell: &gtk::Box, entry: &FileEntry) -> Option<MediaMetadata> {
    let path = entry.path().to_path_buf();
    let (mtime, size) = (modified_secs(entry), entry.info().size() as u64);
    // Only a file not yet read needs the weak references a request holds.
    MediaInfo::shared().get(&path, mtime, size).or_else(|| {
        let (weak_view, weak_cell) = (view.downgrade(), cell.downgrade());
        MediaInfo::shared().request(&path, mtime, size, wanted_for(view, cell, &path), move || {
            let (Some(view), Some(cell)) = (weak_view.upgrade(), weak_cell.upgrade()) else { return };
            let (Some(entry), Some(info)) = (entry_in(&view, &cell), info_label_of(&cell)) else { return };
            update_info(&view, &cell, &entry, &info);
        })
    })
}

glib::wrapper! {
    pub struct BrowseView(ObjectSubclass<imp::BrowseView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl BrowseView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// The folder being listed, if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.imp().path.borrow().clone()
    }

    /// Lists `path` in the background, showing its entries as they arrive. Opening another folder stops
    /// this listing.
    pub fn set_path(&self, path: &Path) {
        let imp = self.imp();
        let scroller = imp.grid.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>();
        if let (Some(old), Some(scroller)) = (imp.path.borrow().clone(), &scroller) {
            imp.scroll_memory.borrow_mut().insert(old, scroller.vadjustment().value());
        }
        let saved = imp.scroll_memory.borrow().get(path).copied().unwrap_or(0.0);
        // Listing the folder already shown again is not entering it.
        let entering = imp.path.borrow().as_deref() != Some(path);
        imp.path.replace(Some(path.to_path_buf()));
        imp.loading.set(true);
        // A folder lists in the order it is read, and is sorted once it is in: sorting each batch as it arrives would cost
        // a sort of everything listed so far, for every batch.
        self.set_sorting(false);
        imp.floating_bar.cancel_hover_hide();
        self.update_status();
        if let Some(store) = imp.store.get() {
            store.remove_all();
        }
        // The grid is detached while the folder loads, so it does not lay out each item as it arrives.
        imp.grid.set_model(None::<&gtk::SingleSelection>);
        if let Some(previous) = imp.reading.take() {
            previous.cancel();
        }
        if imp.search.borrow().lists_tree() {
            self.list_tree(path);
            self.update_monitor();
            return;
        }

        // The folder is listed afresh, since it may have changed since the last visit.
        let files = FileStore::shared();
        files.discard_idle(path);
        let view = self.downgrade();
        let on_batch = {
            let view = view.clone();
            move |batch: &[FileEntry]| {
                let Some(view) = view.upgrade() else { return };
                if let Some(store) = view.imp().store.get() {
                    store.splice(store.n_items(), 0, batch);
                }
            }
        };
        let on_done = move |_: Result<(), String>| {
            let Some(view) = view.upgrade() else { return };
            view.imp().reading.take();
            view.imp().loading.set(false);
            view.set_sorting(true);
            view.attach_selection();
            view.update_status();
            // A selection waiting for the folder brings its item into view, so the saved scroll position does not apply.
            // An arrangement for a folder other than the one that listed is dropped, so it does not run on the wrong one.
            let pending = view
                .imp()
                .pending
                .take()
                .filter(|(folder, _)| view.path().as_deref() == Some(folder.as_path()))
                .map(|(_, run)| run);
            let selecting = pending.is_some();
            if let Some(run) = pending {
                run(&view);
            } else if entering && view.imp().grid.item_count() > 0 {
                // A folder entered with no choice of item starts on its first. One that was scrolled before keeps its
                // place, with the first item selected out of sight.
                if saved > 0.0 {
                    view.imp().grid.select_in_place(0);
                } else {
                    view.imp().grid.select_first();
                }
            }
            if let (Some(scroller), false) = (&scroller, selecting) {
                restore_scroll_after_layout(&view, scroller, saved);
            }
            if view.imp().refresh_skipped.replace(false) {
                view.refresh();
            }
        };
        let reading = files.read(path, on_batch, on_done);
        imp.reading.replace(Some(reading));
        self.update_monitor();
    }

    /// Filters the items by `search`. A search that lists the subfolders lists the whole folder tree under the folder
    /// shown, which takes a while for a large one, and changes back to the folder's own items when it is turned off.
    pub fn set_search(&self, search: Search) {
        let imp = self.imp();
        if *imp.search.borrow() == search {
            return;
        }
        let was_tree = imp.search.borrow().lists_tree();
        let is_tree = search.lists_tree();
        imp.search.replace(search);
        if was_tree != is_tree {
            // Listing the folder again gives what the search needs now: its whole tree, or its own items.
            self.relist();
        }
        self.refresh_filter();
    }

    /// Lists the folder shown afresh, if there is one.
    fn relist(&self) {
        if let Some(path) = self.path() {
            self.set_path(&path);
        }
    }

    /// Lists the folder shown again if its subfolders are searched, since which hidden ones are listed depends on the
    /// setting.
    fn relist_if_tree(&self) {
        if self.imp().search.borrow().lists_tree() {
            self.relist();
        }
    }

    /// Notes whether the viewer is open over the browser. Its items are then not on screen, so thumbnails are no longer
    /// wanted for them, except at the highest aggressiveness, which makes them for the whole folder.
    pub fn set_covered(&self, covered: bool) {
        if self.imp().covered.replace(covered) != covered {
            if covered {
                self.stop_hover();
                self.stop_embedded();
            }
            self.restart_prefetch();
            if !covered {
                self.update_previews_when_shown();
            }
        }
    }

    /// Brings the previews back to the active item once the view is shown again, as the pointer may not move to do it.
    fn update_previews_when_shown(&self) {
        let view = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(view) = view.upgrade() {
                if view.imp().active.get() && !view.imp().covered.get() {
                    view.update_previews();
                }
            }
        });
    }

    /// Makes the embedded preview player ahead of the first preview, along with the crop store, so neither is made while
    /// the pointer waits for the video to start. It waits for the view to settle first.
    fn prewarm_embedded_player(&self) {
        if VideoPreviews::from_name(&self.video_frame_previews()) != VideoPreviews::Embedded {
            return;
        }
        if self.imp().embedded_warmup.borrow().is_some() {
            return;
        }
        let view = self.downgrade();
        let timer = glib::timeout_add_local_once(EMBEDDED_WARMUP_DELAY, move || {
            let Some(view) = view.upgrade() else { return };
            view.imp().embedded_warmup.take();
            if VideoPreviews::from_name(&view.video_frame_previews()) != VideoPreviews::Embedded {
                return;
            }
            let _ = VideoCrops::shared();
            crate::mpv::MpvStream::prewarm_preview(&view.mpv_options());
        });
        self.imp().embedded_warmup.replace(Some(timer));
    }

    /// Whether a thumbnail is wanted for the item at `position`: it is near enough to the items in view for the
    /// aggressiveness of the thumbnail generator, which is the whole folder at the highest, while the view is the one shown.
    fn thumbnail_wanted(&self, position: u32) -> bool {
        let imp = self.imp();
        let Some(pages) = Thumbnails::shared().aggression().pages() else { return imp.active.get() };
        if imp.covered.get() {
            return false;
        }
        let page = imp.grid.page_pixels();
        // A view that has not been laid out yet has nothing to be near to, so nothing is turned away from it.
        page <= 1.0 || imp.grid.distance_from_view(position) <= pages * page + 1.0
    }

    /// Starts asking for thumbnails again, as what they are for changed: the older requests are no longer wanted.
    fn restart_prefetch(&self) {
        let imp = self.imp();
        imp.prefetch_generation.set(imp.prefetch_generation.get() + 1);
        imp.prefetched.borrow_mut().clear();
        imp.prefetch_todo.borrow_mut().clear();
        imp.prefetch_stamps.borrow_mut().clear();
        self.schedule_prefetch();
    }

    /// Asks, in a moment, for the thumbnails of the items near the ones in view that have not been asked for.
    fn schedule_prefetch(&self) {
        let imp = self.imp();
        if imp.prefetch_timer.borrow().is_some() {
            return;
        }
        let view = self.downgrade();
        let timer = glib::timeout_add_local_once(PREFETCH_DELAY, move || {
            if let Some(view) = view.upgrade() {
                view.imp().prefetch_timer.take();
                view.plan_prefetch();
            }
        });
        imp.prefetch_timer.replace(Some(timer));
    }

    /// Works out which items near the ones in view still need their thumbnails asked for, nearest first.
    fn plan_prefetch(&self) {
        let imp = self.imp();
        let level = Thumbnails::shared().aggression();
        let nobody_sees = match level.pages() {
            None => !imp.active.get(),
            Some(_) => imp.covered.get(),
        };
        let near: Vec<(u32, f64)> = if nobody_sees || imp.grid.model().is_none() {
            Vec::new()
        } else {
            match level.pages() {
                Some(pages) => imp.grid.positions_near(Some(pages * imp.grid.page_pixels() + 1.0)),
                None => imp.grid.positions_near(None),
            }
        };
        // What left the reach is forgotten, so it is asked for again if it comes back.
        let reach: std::collections::HashSet<u32> = near.iter().map(|(position, _)| *position).collect();
        let mut asked = imp.prefetched.borrow_mut();
        asked.retain(|position| reach.contains(position));
        let mut todo = imp.prefetch_todo.borrow_mut();
        todo.clear();
        for (position, _) in near {
            if asked.insert(position) {
                todo.push_back(position);
            }
        }
        let waiting = !todo.is_empty();
        drop((asked, todo));
        if waiting {
            self.continue_prefetch();
        }
    }

    /// Asks for the thumbnails of the next few items to ask for, then for more when the main loop is idle, so a large folder
    /// does not hold up the window.
    fn continue_prefetch(&self) {
        let imp = self.imp();
        let generation = imp.prefetch_generation.get();
        for _ in 0..PREFETCH_BATCH {
            let Some(position) = imp.prefetch_todo.borrow_mut().pop_front() else { return };
            self.prefetch_item(position, generation);
        }
        let view = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(view) = view.upgrade().filter(|view| view.imp().prefetch_generation.get() == generation) {
                view.continue_prefetch();
            }
        });
    }

    /// Asks for the thumbnail of the item at `position`, for when its cell is shown, or just to have it ready.
    fn prefetch_item(&self, position: u32, generation: u64) {
        let imp = self.imp();
        let Some(entry) = imp.grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>() else { return };
        let Some(media) = thumbnail_media(&entry) else { return };
        let path = entry.path().to_path_buf();
        let stamp = *imp.prefetch_stamps.borrow_mut().entry(path.clone()).or_insert_with(|| crate::fs::thumbnail::stamp(&path));
        let size = self.item_size();
        let (rank_view, wanted_view, shown_view) = (self.downgrade(), self.downgrade(), self.downgrade());
        let (wanted_path, shown_path) = (path.clone(), path.clone());
        Thumbnails::shared().prefetch_still(
            &path,
            stamp,
            media,
            ThumbnailSize::for_pixels(size * self.scale_factor()),
            move || rank_view.upgrade().map_or(f64::MAX, |view| view.imp().grid.distance_from_view(position)),
            move || {
                wanted_view.upgrade().is_some_and(|view| {
                    let imp = view.imp();
                    imp.prefetch_generation.get() == generation
                        && imp.grid.model().and_then(|model| model.item(position)).and_downcast::<FileEntry>().is_some_and(|entry| entry.path() == wanted_path)
                        && view.thumbnail_wanted(position)
                })
            },
            move |texture| {
                // An item that has its cell by now shows the thumbnail that was made ahead.
                let Some(view) = shown_view.upgrade() else { return };
                let Some(cell) = view.imp().grid.cell_at(position).and_downcast::<gtk::Box>() else { return };
                if !shows(&view, &cell, &shown_path) || view.item_size() != size {
                    return;
                }
                let Some(icon) = icon_of(&cell) else { return };
                icon.set_pixel_size(size);
                icon.set_paintable(Some(&picture_paintable(texture, view.thumbnail_crop(), media == ThumbnailMedia::Video)));
                if let Some(visual) = icon.parent() {
                    visual.add_css_class("has-thumbnail");
                }
            },
        );
    }

    /// Makes this the active view, which watches its folder for changes, or an inactive one, which does not.
    /// An activated view lists its folder again, since it may have changed while it was not watched.
    pub fn set_active(&self, active: bool) {
        let imp = self.imp();
        if imp.active.replace(active) == active {
            return;
        }
        // A view that is not shown gives up its search, so that it does not keep listing its subfolders. The window
        // gives the search back when the view is shown again.
        if !active && imp.search.borrow().lists_tree() {
            self.set_search(Search::default());
        }
        self.restart_prefetch();
        self.update_monitor();
        if active {
            self.refresh();
            self.update_previews_when_shown();
        } else {
            self.stop_hover();
            self.stop_embedded();
        }
    }

    /// Watches the folder shown while the view is active, and stops watching otherwise or when the folder changes.
    fn update_monitor(&self) {
        let imp = self.imp();
        if let Some(timer) = imp.refresh_timer.take() {
            timer.remove();
        }
        if let Some(monitor) = imp.monitor.take() {
            monitor.cancel();
        }
        if !imp.active.get() {
            return;
        }
        let Some(path) = self.path() else { return };
        let Ok(monitor) = gio::File::for_path(&path).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = self.downgrade();
        monitor.connect_changed(move |_, _, _, event| {
            let changed = matches!(
                event,
                gio::FileMonitorEvent::Created
                    | gio::FileMonitorEvent::Deleted
                    | gio::FileMonitorEvent::Renamed
                    | gio::FileMonitorEvent::MovedIn
                    | gio::FileMonitorEvent::MovedOut
                    | gio::FileMonitorEvent::ChangesDoneHint
            );
            if let (true, Some(view)) = (changed, weak.upgrade()) {
                view.schedule_refresh();
            }
        });
        imp.monitor.replace(Some(monitor));
    }

    /// Lists the folder again once it has stopped changing for `REFRESH_DELAY`.
    fn schedule_refresh(&self) {
        let imp = self.imp();
        if let Some(timer) = imp.refresh_timer.take() {
            timer.remove();
        }
        let weak = self.downgrade();
        let timer = glib::timeout_add_local_once(REFRESH_DELAY, move || {
            if let Some(view) = weak.upgrade() {
                view.imp().refresh_timer.take();
                view.refresh();
            }
        });
        imp.refresh_timer.replace(Some(timer));
    }

    /// Lists `root` and every folder under it, one folder at a time, so the items are those of the whole tree, which the
    /// search then picks from. Hidden folders are only read while hidden files are shown.
    fn list_tree(&self, root: &Path) {
        self.imp().loading.set(true);
        self.set_sorting(false);
        let queue = Rc::new(RefCell::new(VecDeque::from([root.to_path_buf()])));
        self.read_tree_folder(queue);
        self.update_status();
    }

    /// Reads the next folder waiting in `queue`, adding the folders in it to the queue, and so on until none are left.
    fn read_tree_folder(&self, queue: Rc<RefCell<VecDeque<PathBuf>>>) {
        let imp = self.imp();
        let next = queue.borrow_mut().pop_front();
        let Some(folder) = next else {
            self.finish_tree_listing();
            return;
        };
        let show_hidden = self.show_hidden();
        let on_batch = {
            let view = self.downgrade();
            let queue = queue.clone();
            move |batch: &[FileEntry]| {
                let Some(view) = view.upgrade() else { return };
                let subfolders = batch
                    .iter()
                    .filter(|entry| entry.is_dir() && (show_hidden || !is_hidden(entry)))
                    .map(|entry| entry.path().to_path_buf());
                queue.borrow_mut().extend(subfolders);
                if let Some(store) = view.imp().store.get() {
                    store.splice(store.n_items(), 0, batch);
                }
            }
        };
        let on_done = {
            let view = self.downgrade();
            move |_: Result<(), String>| {
                // A folder that cannot be read is passed over, so that the rest of the tree is still listed.
                let Some(view) = view.upgrade() else { return };
                view.imp().reading.take();
                view.read_tree_folder(queue.clone());
            }
        };
        // Each folder is read afresh, as the folder shown is, since it may have changed since it was last listed.
        let files = FileStore::shared();
        files.discard_idle(&folder);
        let reading = files.read(&folder, on_batch, on_done);
        imp.reading.replace(Some(reading));
    }

    /// Ends a listing of the whole tree, as `set_path` ends one: the items are sorted, and an arrangement waiting for the
    /// folder runs, or else the first item is selected.
    fn finish_tree_listing(&self) {
        let imp = self.imp();
        imp.loading.set(false);
        self.set_sorting(true);
        self.attach_selection();
        let pending = imp
            .pending
            .take()
            .filter(|(folder, _)| self.path().as_deref() == Some(folder.as_path()))
            .map(|(_, run)| run);
        if let Some(run) = pending {
            run(self);
        } else if self.item_count() > 0 {
            self.select_first();
        }
        self.update_status();
    }

    /// Lists the folder afresh and updates the items to match, keeping the selection on the same item. Does
    /// nothing while the folder is loading, since that listing is already fresh.
    fn refresh(&self) {
        let imp = self.imp();
        // The subfolders' items are not the folder's listing, so a change in the folder does not reconcile them.
        if imp.search.borrow().lists_tree() {
            return;
        }
        if imp.reading.borrow().is_some() {
            imp.refresh_skipped.set(true);
            return;
        }
        let Some(path) = self.path() else { return };
        let files = FileStore::shared();
        files.discard_idle(&path);
        let listed = Rc::new(RefCell::new(Vec::new()));
        let on_batch = {
            let listed = listed.clone();
            move |batch: &[FileEntry]| listed.borrow_mut().extend_from_slice(batch)
        };
        let on_done = {
            let view = self.downgrade();
            move |result: Result<(), String>| {
                let Some(view) = view.upgrade() else { return };
                view.imp().reading.take();
                if result.is_ok() {
                    view.reconcile(&listed.borrow());
                }
                if view.imp().refresh_skipped.replace(false) {
                    view.refresh();
                }
            }
        };
        let reading = files.read(&path, on_batch, on_done);
        imp.reading.replace(Some(reading));
    }

    /// Makes the items match `listed`: entries that are gone are removed, new or changed ones are added, and
    /// unchanged ones keep their objects, so their thumbnails stay.
    fn reconcile(&self, listed: &[FileEntry]) {
        let imp = self.imp();
        let Some(store) = imp.store.get() else { return };
        let selected = imp
            .grid
            .model()
            .and_then(|model| model.item(model.selected()).and_downcast::<FileEntry>())
            .map(|entry| entry.name());

        // Entries are matched by path, which unlike a display name is unique within a folder.
        let mut wanted: std::collections::HashMap<PathBuf, FileEntry> =
            listed.iter().map(|entry| (entry.path().to_path_buf(), entry.clone())).collect();
        let mut removals = Vec::new();
        let mut position = store.n_items();
        while position > 0 {
            position -= 1;
            let Some(shown) = store.item(position).and_downcast::<FileEntry>() else { continue };
            let unchanged = wanted
                .get(shown.path())
                .is_some_and(|new| new.modified() == shown.modified() && new.size() == shown.size());
            if unchanged {
                wanted.remove(shown.path());
            } else {
                removals.push(position);
            }
        }
        let added: Vec<FileEntry> = wanted.into_values().collect();
        let changed = !removals.is_empty() || !added.is_empty();
        // One change to the grid for the whole batch, so each change does not lay out and mark-prune the grid again.
        imp.grid.batch_changes(|| {
            for (first, count) in runs(&removals) {
                store.splice(first, count, &[] as &[FileEntry]);
            }
            store.splice(store.n_items(), 0, &added);
        });

        // Selecting by name here, rather than with `select_named`, leaves the scroll where it is.
        if let (Some(name), Some(model)) = (selected, imp.grid.model()) {
            let position = (0..model.n_items())
                .find(|&position| model.item(position).and_downcast::<FileEntry>().is_some_and(|entry| entry.name() == name));
            if let Some(position) = position {
                model.set_selected(position);
            }
        }
        self.update_status();
        if changed {
            self.emit_by_name::<()>("listing-changed", &[]);
        }
    }

    /// Calls `callback` when the folder's items change as it is listed again, so a viewer showing them can follow.
    pub fn connect_listing_changed<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("listing-changed", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            callback(&view);
            None
        })
    }

    pub fn set_info_fields(&self, fields: &[InfoField]) {
        self.imp().info_fields.replace(fields.to_vec());
        self.refresh_items();
    }

    /// The number of items shown, after the hidden filter.
    pub fn item_count(&self) -> u32 {
        self.imp().grid.model().map_or(0, |model| model.n_items())
    }

    /// The names of the items shown, in display order.
    pub fn item_names(&self) -> Vec<String> {
        let Some(model) = self.imp().grid.model() else {
            return Vec::new();
        };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .map(|entry| entry.name())
            .collect()
    }

    /// Moves the cursor to the first item.
    pub fn select_first(&self) {
        self.imp().grid.select_first();
        self.keyboard_moved();
    }

    /// Moves the cursor to the last item.
    pub fn select_last(&self) {
        self.imp().grid.select_last();
        self.keyboard_moved();
    }

    /// Marks every item in the folder, as select-all does.
    pub fn select_all(&self) {
        self.imp().grid.mark_all();
    }

    /// The paths of the marked items, in the order they are listed.
    pub fn marked_paths(&self) -> Vec<PathBuf> {
        self.imp().grid.marked_paths()
    }

    /// Shows again which of the items are in the playlist, as it has changed.
    pub fn refresh_playlist_marks(&self) {
        self.imp().grid.update_selected_classes();
    }

    /// Moves the keyboard focus to the grid of items. Returns whether it took it.
    pub fn focus_grid(&self) -> bool {
        self.imp().grid.grab_focus()
    }

    /// Opens the item under the cursor.
    pub fn activate_selected(&self) {
        self.imp().grid.activate_selected();
    }

    /// Moves the cursor `items` along the list: negative moves back.
    pub fn move_cursor_x(&self, items: i32) {
        self.imp().grid.move_cursor_x(items);
        self.keyboard_moved();
    }

    /// Moves the cursor `rows` rows down the grid: negative moves up.
    pub fn move_cursor_y(&self, rows: i32) {
        self.imp().grid.move_cursor_y(rows);
        self.keyboard_moved();
    }

    /// Moves the cursor `pages` pages down the grid: negative moves up.
    pub fn move_cursor_page(&self, pages: i32) {
        self.imp().grid.move_cursor_page(pages);
        self.keyboard_moved();
    }

    /// The items shown, in display order.
    pub fn entries(&self) -> Vec<ListedFile> {
        let Some(model) = self.imp().grid.model() else { return Vec::new() };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .map(|entry| ListedFile { name: entry.name(), path: entry.path().to_path_buf(), is_dir: entry.is_dir() })
            .collect()
    }

    /// Selects the item at `index`, counting from the start, or from the end for a negative index. Returns the
    /// name of the item selected, or `None` if there is no item at that index.
    pub fn select_at(&self, index: i64) -> Option<String> {
        let model = self.imp().grid.model()?;
        let count = i64::from(model.n_items());
        let position = if index < 0 { count + index } else { index };
        if !(0..count).contains(&position) {
            return None;
        }
        let position = position as u32;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// Arranges for `run` to happen once `folder` has listed, if it is still the folder shown then. A later
    /// arrangement replaces it.
    pub fn set_pending(&self, folder: &Path, run: impl FnOnce(&BrowseView) + 'static) {
        self.imp().pending.replace(Some((folder.to_path_buf(), Box::new(run))));
    }

    /// Sets the items sorted, or not: while a folder lists, its items are left in the order they arrive.
    fn set_sorting(&self, sorting: bool) {
        let imp = self.imp();
        let (Some(sorted), Some(sorter)) = (imp.sorted.get(), imp.sorter.get()) else { return };
        sorted.set_sorter(if sorting { Some(sorter) } else { None::<&gtk::CustomSorter> });
    }

    /// Whether something is arranged for once `folder` has listed.
    pub fn has_pending_for(&self, folder: &Path) -> bool {
        self.imp().pending.borrow().as_ref().is_some_and(|(pending, _)| pending == folder)
    }

    /// Drops whatever was arranged with `set_pending`.
    pub fn clear_pending(&self) {
        self.imp().pending.take();
    }

    /// Selects the first picture or video in the folder, or the last one if `last` is set. Returns its name.
    pub fn select_media(&self, last: bool) -> Option<String> {
        let model = self.imp().grid.model()?;
        let is_media_entry = |position: u32| {
            model.item(position).and_downcast::<FileEntry>().is_some_and(|entry| !entry.is_dir() && is_media(&entry))
        };
        let positions: Vec<u32> = (0..model.n_items()).filter(|&position| is_media_entry(position)).collect();
        let position = if last { positions.last() } else { positions.first() }.copied()?;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// The position of the selected item, if one is selected.
    pub fn selected_position(&self) -> Option<u32> {
        let model = self.imp().grid.model()?;
        let position = model.selected();
        (position < model.n_items()).then_some(position)
    }

    /// The name of the selected item, if one is selected.
    pub fn selected_name(&self) -> Option<String> {
        let model = self.imp().grid.model()?;
        model.item(model.selected()).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// Selects the item whose name or path is `file`, and returns the name of the item selected.
    pub fn select_named(&self, file: &str) -> Option<String> {
        let model = self.imp().grid.model()?;
        let position = (0..model.n_items()).find(|&position| {
            model
                .item(position)
                .and_downcast::<FileEntry>()
                .is_some_and(|entry| entry.name() == file || entry.path().to_string_lossy() == file)
        })?;
        self.imp().grid.select(position);
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.name())
    }

    /// The pictures and videos shown, in display order: the items the viewer steps through.
    pub fn media_paths(&self) -> Vec<PathBuf> {
        let Some(model) = self.imp().grid.model() else {
            return Vec::new();
        };
        (0..model.n_items())
            .filter_map(|position| model.item(position).and_downcast::<FileEntry>())
            .filter(|entry| is_media(entry))
            .map(|entry| entry.path().to_path_buf())
            .collect()
    }

    /// Opens the item at `position`: a folder asks to open it, a file asks to open it in the viewer.
    pub fn activate(&self, position: u32) {
        let Some(entry) = self
            .imp()
            .grid
            .model()
            .and_then(|model| model.item(position))
            .and_downcast::<FileEntry>()
        else {
            return;
        };
        let path = entry.path().to_string_lossy().into_owned();
        let signal = if entry.is_dir() { "directory-activated" } else { "file-activated" };
        self.emit_by_name::<()>(signal, &[&path]);
    }

    /// Calls `callback(view, name)` when the selected item changes while the folder is shown, with the item's name,
    /// or an empty name when none is selected.
    pub fn connect_selection_changed<F: Fn(&Self, &str) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("selection-changed", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let name = values[1].get::<String>().expect("name argument");
            callback(&view, &name);
            None
        })
    }

    /// Calls `callback(view, path)` when a folder is activated.
    pub fn connect_directory_activated<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("directory-activated", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    /// Calls `callback(view, path, is_dir, x, y)` when an item is right-clicked. The pointer is in the view's
    /// coordinates.
    /// Calls `callback(view, path)` when a folder is middle-clicked, with its absolute path.
    pub fn connect_open_in_new_tab_requested<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("open-in-new-tab-requested", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    pub fn connect_item_context_requested<F>(&self, callback: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, &Path, bool, f64, f64) + 'static,
    {
        self.connect_local("item-context-requested", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            let is_dir = values[2].get::<bool>().expect("is_dir argument");
            let x = values[3].get::<f64>().expect("x argument");
            let y = values[4].get::<f64>().expect("y argument");
            callback(&view, Path::new(&path), is_dir, x, y);
            None
        })
    }

    /// Calls `callback(view, path)` when a file is activated.
    pub fn connect_file_activated<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("file-activated", false, move |values| {
            let view = values[0].get::<BrowseView>().expect("view argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&view, Path::new(&path));
            None
        })
    }

    /// Re-applies the hidden and media filter after either changes.
    fn refresh_filter(&self) {
        if let Some(filter) = self.imp().filter.get() {
            filter.changed(gtk::FilterChange::Different);
        }
        self.update_status();
    }

    /// Re-sorts the items after the sort key, its direction or the folders' placement changes.
    fn resort(&self) {
        let Some(sorter) = self.imp().sorter.get() else { return };
        // The item selected before the sort stays selected, wherever it lands, and is scrolled into view.
        let selected = self
            .imp()
            .grid
            .model()
            .and_then(|model| model.item(model.selected()).and_downcast::<FileEntry>())
            .map(|entry| entry.name());
        sorter.changed(gtk::SorterChange::Different);
        if let Some(name) = selected {
            self.select_named(&name);
        }
    }

    /// Notes that the selection was moved from the keyboard: the pointer hides once it has been still for a while.
    fn keyboard_moved(&self) {
        let imp = self.imp();
        imp.keyboard_browsing.set(true);
        self.tooltip_leave();
        if let Some(timer) = imp.keyboard_timer.take() {
            timer.remove();
        }
        let view = self.downgrade();
        let timer = glib::timeout_add_local_once(KEYBOARD_IDLE_DELAY, move || {
            if let Some(view) = view.upgrade() {
                view.hide_idle_pointer();
            }
        });
        *imp.keyboard_timer.borrow_mut() = Some(timer);
    }

    /// Hides the pointer, after the keyboard moved the selection and the pointer has been still.
    fn hide_idle_pointer(&self) {
        self.imp().keyboard_timer.take();
        self.set_cursor(gtk::gdk::Cursor::from_name("none", None).as_ref());
    }

    /// Opens the description of the active item now, as hovering it would after a moment.
    pub fn show_tooltip_now(&self) {
        if let Some(cell) = self.active_cell() {
            self.tooltip_show(&cell);
        }
    }

    /// Shows the pointer again, and ends keyboard browsing, once the pointer moves.
    fn pointer_moved(&self) {
        let imp = self.imp();
        // The pointer has moved since the selection did, so the item under it is the active one again.
        if imp.follow_selection.replace(false) {
            self.update_previews();
        }
        if !imp.keyboard_browsing.replace(false) {
            return;
        }
        if let Some(timer) = imp.keyboard_timer.take() {
            timer.remove();
        }
        self.set_cursor(None);
        self.tooltip_leave();
    }

    /// Stops loading the folder: the entries read so far are shown.
    fn stop_loading(&self) {
        let imp = self.imp();
        if let Some(reading) = imp.reading.take() {
            reading.cancel();
        }
        imp.loading.set(false);
        self.set_sorting(true);
        self.attach_selection();
        self.update_status();
    }

    /// Shows the selection's items in the grid again.
    fn attach_selection(&self) {
        let imp = self.imp();
        if let Some(selection) = imp.selection.get() {
            imp.grid.set_model(Some(selection));
        }
    }

    /// Updates the floating bar like Nautilus's: while a folder loads, a spinner and a stop button;
    /// with an item selected, what it is and its size; otherwise the bar is hidden.
    fn update_status(&self) {
        let imp = self.imp();
        if imp.loading.get() {
            imp.floating_bar.set_labels(Some("Loading…"), None);
            imp.floating_bar.set_loading(true);
            imp.floating_bar.set_visible(true);
            return;
        }
        imp.floating_bar.set_loading(false);
        let marked = imp.grid.marked_count();
        if marked > 1 {
            imp.floating_bar.set_labels(Some(&format!("{marked} items selected")), None);
            imp.floating_bar.set_visible(true);
            return;
        }
        let selected = imp.selection.get().and_then(|selection| selection.selected_item());
        let Some(entry) = selected.and_downcast::<FileEntry>() else {
            imp.floating_bar.set_visible(false);
            return;
        };
        if entry.is_dir() {
            let primary = format!("“{}” selected", entry.name());
            imp.floating_bar.set_labels(Some(&primary), None);
        } else {
            let size = glib::format_size(entry.info().size() as u64);
            imp.floating_bar.set_labels(Some("1 item selected"), Some(&format!("({size})")));
        }
        imp.floating_bar.set_visible(true);
    }

    /// The height of one item: the visual, then the name and info lines as the settings lay them out.
    fn cell_height(&self) -> i32 {
        let builder = gtk::Builder::from_resource("/au/topher/rsimgv/ui/browse_item.ui");
        let (Some(cell), Some(visual), Some(name), Some(info)) = (
            builder.object::<gtk::Box>("cell"),
            builder.object::<gtk::Box>("visual"),
            builder.object::<gtk::Label>("name"),
            builder.object::<gtk::Label>("info"),
        ) else {
            return self.item_size();
        };
        // The visual is the item's square, as style_item sizes it.
        visual.set_size_request(self.item_size(), self.item_size());
        cell.set_size_request(self.item_size(), -1);
        // Two lines of text are measured when names wrap, when info is stacked, and when inline info has both ordinary
        // fields and viewing-history fields, which start on a line of their own.
        let name_lines = if self.filename_display() == "wrapped" { "Ag\nAg" } else { "Ag" };
        name.set_label(name_lines);
        name.set_visible(self.filename_display() != "off");
        let fields = self.imp().info_fields.borrow();
        let two_info_lines = self.info_layout() == "stacked"
            || (fields.iter().any(|field| field.is_watch()) && fields.iter().any(|field| !field.is_watch()));
        let info_lines = if two_info_lines { "Ag\nAg" } else { "Ag" };
        info.set_label(info_lines);
        info.set_visible(self.info_layout() != "disabled");
        cell.measure(gtk::Orientation::Vertical, self.item_size()).1
    }

    /// Opens the description of the item in `cell` below it, once the pointer has rested there.
    fn tooltip_enter(&self, cell: &gtk::Box) {
        self.tooltip_leave();
        let view = self.downgrade();
        let cell = cell.downgrade();
        let timer = glib::timeout_add_local_once(TOOLTIP_DELAY, move || {
            if let (Some(view), Some(cell)) = (view.upgrade(), cell.upgrade()) {
                view.tooltip_show(&cell);
            }
        });
        *self.imp().tooltip_timer.borrow_mut() = Some(timer);
    }

    /// Closes the description, and cancels it if it has not opened yet.
    fn tooltip_leave(&self) {
        if let Some(timer) = self.imp().tooltip_timer.take() {
            timer.remove();
        }
        if let Some(popover) = self.imp().tooltip.get() {
            popover.popdown();
        }
    }

    fn tooltip_show(&self, cell: &gtk::Box) {
        // The timer has fired, so there is nothing left to cancel.
        self.imp().tooltip_timer.take();
        let (Some(popover), Some(entry)) = (self.imp().tooltip.get(), entry_in(self, cell)) else { return };
        let metadata = if thumbnail_media(&entry).is_some() { request_metadata(self, cell, &entry) } else { None };
        popover.set_child(Some(&tooltip_content(&entry, metadata.as_ref())));
        if popover.parent().is_some() {
            popover.unparent();
        }
        popover.set_parent(cell);
        // Anchored to the thumbnail's container, so the popover opens directly below it.
        if let Some(bounds) = cell.first_child().and_then(|visual| visual.compute_bounds(cell)) {
            let thumbnail = gtk::gdk::Rectangle::new(
                bounds.x() as i32,
                bounds.y() as i32,
                bounds.width() as i32,
                bounds.height() as i32,
            );
            popover.set_pointing_to(Some(&thumbnail));
        }
        popover.popup();
    }

    /// Cycles the preview pictures of the folder that is the active item, and stops cycling any other.
    fn update_cycle(&self) {
        let target = if self.folder_preview_cycle() { self.cycle_target() } else { None };
        let cycling = self.imp().cycle.borrow().as_ref().and_then(|cycle| cycle.cell.upgrade());
        if target.is_some() && target == cycling {
            return;
        }
        self.stop_cycle();
        if let Some(cell) = target {
            self.start_cycle(&cell);
        }
    }

    /// The cell of the item the previews belong to: whichever the user last chose, the item under the pointer or the
    /// one selected. The pointer resting over an item does not take it back from a selection made since.
    fn active_cell(&self) -> Option<gtk::Box> {
        let imp = self.imp();
        let hovered = imp.hovered_cell.borrow().as_ref().and_then(|cell| cell.upgrade());
        let selected = imp
            .grid
            .model()
            .and_then(|model| imp.grid.cell_at(model.selected()))
            .and_then(|widget| widget.downcast::<gtk::Box>().ok());
        if imp.follow_selection.get() { selected.or(hovered) } else { hovered.or(selected) }
    }

    /// Brings the folder cycle and the video frames to the active item.
    fn update_previews(&self) {
        self.update_cycle();
        self.update_video_preview();
    }

    /// The cell of the folder to cycle: the active item, if it is a folder.
    fn cycle_target(&self) -> Option<gtk::Box> {
        self.active_cell().filter(|cell| self.is_folder_cell(cell))
    }

    /// The most pictures scattered over a folder's icon, at least one.
    fn preview_count(&self) -> usize {
        self.folder_preview_count().max(1) as usize
    }

    /// Whether `cell` shows a folder, and folder previews are on.
    fn is_folder_cell(&self, cell: &gtk::Box) -> bool {
        self.directory_previews() && entry_in(self, cell).is_some_and(|entry| entry.is_dir())
    }

    /// Reads the sort and filter settings from the properties into plain values, which the sorter and filter read on
    /// each comparison without allocating. Called when any of them changes.
    fn cache_settings(&self) {
        let imp = self.imp();
        let by = match self.sort_by().as_str() {
            "modified" => SortBy::Modified,
            "size" => SortBy::Size,
            "type" => SortBy::Type,
            _ => SortBy::Name,
        };
        let folders = self.show_directories();
        imp.order.set(Order {
            by,
            descending: self.sort_order() == "descending",
            inline_folders: folders == "inline",
        });
        imp.visibility.set(Visibility {
            show_hidden: self.show_hidden(),
            media_only: self.media_only(),
            show_images: self.show_images(),
            show_videos: self.show_videos(),
            folders_listed: folders != "none",
        });
    }

    /// The pictures and videos scanned for `folder`'s previews, if the scan is kept and the folder has not changed since
    /// it was made, which its modification time `modified` tells.
    fn cached_scan(&self, folder: &Path, modified: i64) -> Option<Rc<Vec<Picture>>> {
        self.imp().folder_scans.borrow_mut().get(folder, modified)
    }

    /// Starts cycling the previews of the folder in `cell`, if they are up and there is more than one picture to cycle.
    fn start_cycle(&self, cell: &gtk::Box) {
        let Some(entry) = entry_in(self, cell) else { return };
        let Some(scattered) = scattered_in(cell) else { return };
        let folder = entry.path().to_path_buf();
        // The folder's pictures come from its scan, which its cell made when it was bound; without it there is nothing to cycle yet.
        let Some(media) = self.cached_scan(&folder, modified_secs(&entry)) else { return };
        if media.len() < 2 {
            return;
        }
        let fitted = self.folder_preview_fit();
        let slots = if fitted { MAX_PHOTOS } else { self.preview_count() };
        let mut cycle = Cycle {
            cell: cell.downgrade(),
            folder,
            media,
            scattered,
            slots,
            fitted,
            size: self.item_size(),
            offset: 0,
            preload: None,
        };
        // The page after the first starts loading straight away, so it is ready when the cycle reaches it.
        let next = following(0, self.page_length(cell, &cycle.folder, &cycle.media, 0, fitted, slots), cycle.media.len());
        cycle.preload = Some(self.request_page(cell, &cycle, next));
        *self.imp().cycle.borrow_mut() = Some(cycle);
        let view = self.downgrade();
        let timer = glib::timeout_add_local(CYCLE_INTERVAL, move || match view.upgrade() {
            Some(view) => view.advance_cycle(),
            None => glib::ControlFlow::Break,
        });
        self.imp().cycle_timer.replace(Some(timer));
    }

    /// Moves the cycling folder on to its next pictures, or stops if its cell no longer shows it.
    fn advance_cycle(&self) -> glib::ControlFlow {
        let Some(mut cycle) = self.imp().cycle.borrow_mut().take() else {
            self.imp().cycle_timer.take();
            return glib::ControlFlow::Break;
        };
        let Some(cell) = cycle.cell.upgrade().filter(|cell| self.still_cycling(cell, &cycle)) else {
            self.imp().cycle_timer.take();
            return glib::ControlFlow::Break;
        };
        // The next page is shown only once all its pictures are made; until then the cycle waits on it.
        let preload = match cycle.preload.take() {
            Some(preload) if preload.borrow().remaining == 0 => preload,
            waiting => {
                cycle.preload = waiting;
                *self.imp().cycle.borrow_mut() = Some(cycle);
                return glib::ControlFlow::Continue;
            }
        };
        let (offset, len, page) = {
            let mut preload = preload.borrow_mut();
            (preload.offset, preload.len, std::mem::take(&mut preload.page))
        };
        cycle.offset = offset;
        cycle.scattered.set_page(page);
        // The page after this one starts loading at once.
        let next = following(offset, len, cycle.media.len());
        cycle.preload = Some(self.request_page(&cell, &cycle, next));
        *self.imp().cycle.borrow_mut() = Some(cycle);
        glib::ControlFlow::Continue
    }

    /// How many of a folder's pictures a page from `offset` takes: in fitted mode, as many as fit their dimensions,
    /// otherwise up to `slots`.
    fn page_length(&self, cell: &gtk::Box, folder: &Path, media: &[Picture], offset: usize, fitted: bool, slots: usize) -> usize {
        let available = media.len().saturating_sub(offset);
        if !fitted {
            return slots.min(available);
        }
        fitted_length(media.get(offset..).unwrap_or_default(), |(path, modified, size, _)| {
            self.aspect_of(cell, folder, path, *modified, *size)
        })
    }

    /// The width over the height of the picture at `path`. Its metadata is read in the background if it is not known
    /// yet, and the folder's previews are laid out again once it is; until then it counts as square.
    fn aspect_of(&self, cell: &gtk::Box, folder: &Path, path: &Path, modified: i64, size: u64) -> f32 {
        // Only a file not yet read needs the weak references a request holds.
        let metadata = MediaInfo::shared().get(path, modified, size).or_else(|| {
            let (weak_view, weak_cell) = (self.downgrade(), cell.downgrade());
            MediaInfo::shared().request(path, modified, size, wanted_for(self, cell, folder), move || {
                let (Some(view), Some(cell)) = (weak_view.upgrade(), weak_cell.upgrade()) else { return };
                if let Some(entry) = entry_in(&view, &cell) {
                    relayout_scattered(&view, &cell, &entry);
                }
            })
        });
        aspect_ratio(metadata)
    }

    /// Starts making the pictures of the page from `offset`. The cycle shows the page once they are all made.
    fn request_page(&self, cell: &gtk::Box, cycle: &Cycle, offset: usize) -> Rc<RefCell<Preload>> {
        let len = self.page_length(cell, &cycle.folder, &cycle.media, offset, cycle.fitted, cycle.slots);
        let preload = Rc::new(RefCell::new(Preload {
            offset,
            len,
            page: vec![None; cycle.slots],
            remaining: len,
        }));
        for slot in 0..len {
            let (path, modified, _, kind) = &cycle.media[offset + slot];
            let turn = photo_turn(path);
            let preload = preload.clone();
            Thumbnails::shared().request_still_or_failed(
                path,
                *modified,
                *kind,
                photo_size(cycle.size, cell.scale_factor()),
                wanted_for(self, cell, &cycle.folder),
                move |texture| {
                    let mut preload = preload.borrow_mut();
                    // A picture that cannot be made leaves its slot empty, but still counts as made, so the cycle moves on.
                    if let Some(texture) = texture {
                        preload.page[slot] = Some((texture.clone(), turn));
                    }
                    preload.remaining -= 1;
                },
            );
        }
        preload
    }

    /// Whether `cell` still shows the cycling folder, with the same previews it had when the cycle started.
    fn still_cycling(&self, cell: &gtk::Box, cycle: &Cycle) -> bool {
        shows(self, cell, &cycle.folder) && scattered_in(cell).is_some_and(|scattered| scattered == cycle.scattered)
    }

    /// Shows the cycle's pictures from its offset in `cell`, each as its thumbnail is made.
    fn show_cycle(&self, cycle: &Cycle, cell: &gtk::Box) {
        let page = self.page_length(cell, &cycle.folder, &cycle.media, cycle.offset, cycle.fitted, cycle.slots);
        for slot in 0..cycle.slots {
            let scattered = cycle.scattered.clone();
            if slot >= page {
                scattered.clear_photo(slot);
                continue;
            }
            let (path, modified, _, kind) = &cycle.media[cycle.offset + slot];
            let turn = photo_turn(path);
            Thumbnails::shared().request_still(
                path,
                *modified,
                *kind,
                photo_size(cycle.size, cell.scale_factor()),
                wanted_for(self, cell, &cycle.folder),
                move |texture| scattered.set_photo(slot, texture, turn),
            );
        }
    }

    /// Stops cycling the folder, if one is, and puts its pictures back to the first ones.
    fn stop_cycle(&self) {
        if let Some(timer) = self.imp().cycle_timer.take() {
            timer.remove();
        }
        let Some(mut cycle) = self.imp().cycle.take() else { return };
        if let Some(cell) = cycle.cell.upgrade().filter(|cell| self.still_cycling(cell, &cycle)) {
            cycle.offset = 0;
            self.show_cycle(&cycle, &cell);
        }
    }

    /// Makes the item under the pointer the active one.
    fn hover_enter(&self, cell: &gtk::Box) {
        self.imp().hovered_cell.replace(Some(cell.downgrade()));
        self.update_previews();
    }

    /// Whether `cell` shows a video.
    fn is_video_cell(&self, cell: &gtk::Box) -> bool {
        entry_in(self, cell).is_some_and(|entry| thumbnail_media(&entry) == Some(ThumbnailMedia::Video))
    }

    /// Plays the preview of the video in the active item, in the way the setting asks, and stops any other preview.
    fn update_video_preview(&self) {
        let mode = VideoPreviews::from_name(&self.video_frame_previews());
        if mode != VideoPreviews::Frames {
            self.stop_hover();
        }
        if mode != VideoPreviews::Embedded {
            self.stop_embedded();
        }
        match mode {
            VideoPreviews::None => {}
            VideoPreviews::Frames => self.update_frame_preview(),
            VideoPreviews::Embedded => self.update_embedded_preview(),
        }
    }

    /// Plays the frames of the video in the active item, and stops the ones playing in any other.
    fn update_frame_preview(&self) {
        let target = self
            .active_cell()
            .filter(|cell| self.is_video_cell(cell))
            .and_then(|cell| entry_in(self, &cell).map(|entry| (cell, entry.path().to_path_buf())));
        let playing = self.imp().hovered.borrow().as_ref().map(|hover| (hover.cell.upgrade(), hover.path.clone()));
        if let (Some((cell, path)), Some((Some(playing_cell), playing_path))) = (&target, &playing) {
            if cell == playing_cell && path == playing_path {
                return;
            }
        }
        self.stop_hover();
        if let Some((cell, _)) = target {
            self.play_video_frames(&cell);
        }
    }

    /// Starts the embedded player on the video in the active item, and stops one playing in any other. The video waits a
    /// moment first, so sweeping the pointer across the items loads only the one it stops on.
    fn update_embedded_preview(&self) {
        let target = self
            .active_cell()
            .filter(|cell| self.is_video_cell(cell))
            .and_then(|cell| entry_in(self, &cell).map(|entry| (cell, entry.path().to_path_buf())));
        let playing = self.imp().embedded.borrow().as_ref().map(|embedded| (embedded.cell.upgrade(), embedded.path.clone()));
        if let (Some((cell, path)), Some((Some(playing_cell), playing_path))) = (&target, &playing) {
            if cell == playing_cell && path == playing_path {
                return;
            }
        }
        let waiting = self.imp().pending_embedded.borrow().as_ref().is_some_and(|(waiting_cell, waiting_path)| {
            target.as_ref().is_some_and(|(cell, path)| waiting_cell.upgrade().as_ref() == Some(cell) && waiting_path == path)
        });
        if waiting {
            return;
        }
        self.stop_embedded();
        if let Some((cell, _)) = target {
            self.schedule_embedded(&cell);
        }
    }

    /// Starts the embedded preview on `cell` once it has stayed the active item for a moment.
    fn schedule_embedded(&self, cell: &gtk::Box) {
        let Some(entry) = entry_in(self, cell) else { return };
        let path = entry.path().to_path_buf();
        self.imp().pending_embedded.replace(Some((cell.downgrade(), path.clone())));
        let view = self.downgrade();
        let weak_cell = cell.downgrade();
        let timer = glib::timeout_add_local_once(EMBEDDED_START_DELAY, move || {
            let Some(view) = view.upgrade() else { return };
            view.imp().embedded_timer.take();
            view.imp().pending_embedded.take();
            if VideoPreviews::from_name(&view.video_frame_previews()) != VideoPreviews::Embedded
                || view.imp().covered.get()
                || !view.imp().active.get()
            {
                return;
            }
            let Some(cell) = weak_cell.upgrade() else { return };
            if shows(&view, &cell, &path) {
                view.play_embedded(&cell);
            }
        });
        self.imp().embedded_timer.replace(Some(timer));
    }

    /// Starts playing the frames of the video in `cell`, once they are made.
    fn play_video_frames(&self, cell: &gtk::Box) {
        let Some(entry) = entry_in(self, cell) else { return };
        let imp = self.imp();
        let generation = imp.hover_generation.get() + 1;
        imp.hover_generation.set(generation);
        let path = entry.path().to_path_buf();
        imp.hovered.replace(Some(Hover {
            cell: cell.downgrade(),
            path: path.clone(),
            generation,
        }));
        let view = self.downgrade();
        let weak_cell = cell.downgrade();
        // The frames are wanted only while this hover lasts: once the pointer has moved on, they are not made.
        let still_wanted = wanted_for(self, cell, &path);
        let hover_view = self.downgrade();
        Thumbnails::shared().request_frames(
            &path,
            crate::fs::thumbnail::stamp(&path),
            ThumbnailSize::for_pixels(self.item_size() * cell.scale_factor()),
            move || {
                still_wanted()
                    && hover_view.upgrade().is_some_and(|view| is_current_hover(&view.imp().hovered.borrow(), generation))
            },
            move |frames| {
                let (Some(view), Some(cell)) = (view.upgrade(), weak_cell.upgrade()) else { return };
                // Empty when the frames cannot be made, such as a video that ffmpeg cannot read.
                if !frames.is_empty() {
                    view.start_playing(&cell, generation, frames.to_vec());
                }
            },
        );
    }

    /// Plays `frames` in `cell`, one every `FRAME_INTERVAL`, until the hover ends or the cell shows
    /// another item.
    fn start_playing(&self, cell: &gtk::Box, generation: u64, frames: Vec<gtk::gdk::Texture>) {
        if !is_current_hover(&self.imp().hovered.borrow(), generation) {
            return;
        }
        // Each frame is drawn between film strips, as a video's still is.
        let crop = self.thumbnail_crop();
        let frames: Vec<gtk::gdk::Paintable> = frames.iter().map(|texture| picture_paintable(texture, crop, true)).collect();
        let view = self.downgrade();
        let weak_cell = cell.downgrade();
        let mut index = 0;
        glib::timeout_add_local(FRAME_INTERVAL, move || {
            let (Some(view), Some(cell)) = (view.upgrade(), weak_cell.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            let path = {
                let hovered = view.imp().hovered.borrow();
                match hovered.as_ref().filter(|hover| hover.generation == generation) {
                    Some(hover) => hover.path.clone(),
                    None => return glib::ControlFlow::Break,
                }
            };
            if !shows(&view, &cell, &path) {
                return glib::ControlFlow::Break;
            }
            index = (index + 1) % frames.len();
            if let Some(icon) = icon_of(&cell) {
                icon.set_paintable(Some(&frames[index]));
            }
            glib::ControlFlow::Continue
        });
    }

    /// Hands the active item back to the selected one when the pointer leaves `cell`. That waits for the events being
    /// handled, as a pointer going from one item to the next leaves the first before it enters the second.
    fn hover_leave(&self, cell: &gtk::Box) {
        let left_hovered = self.imp().hovered_cell.borrow().as_ref().and_then(|hovered| hovered.upgrade()).is_some_and(|hovered| &hovered == cell);
        if left_hovered {
            self.imp().hovered_cell.take();
            let view = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(view) = view.upgrade() {
                    view.update_previews();
                }
            });
        }
    }

    /// Ends the hover playback, if any, and shows the still thumbnail of its video again.
    fn stop_hover(&self) {
        let Some(hover) = self.imp().hovered.take() else { return };
        let Some(cell) = hover.cell.upgrade() else { return };
        let (Some(entry), Some(icon)) = (entry_in(self, &cell), icon_of(&cell)) else { return };
        if let Some(media) = thumbnail_media(&entry) {
            request_thumbnail(self, &cell, &entry, &icon, self.item_size(), media);
        }
    }

    /// Plays the video in `cell` in an embedded player, cropped to the crop locked for it and filling the item when
    /// thumbnails are cropped to a square. The player is kept and reused for the next preview.
    fn play_embedded(&self, cell: &gtk::Box) {
        let Some(entry) = entry_in(self, cell) else { return };
        let Some(player) = player_of(cell) else { return };
        let path = entry.path().to_path_buf();
        let Some(stream) = crate::mpv::MpvStream::for_preview(&path, &self.mpv_options()) else { return };
        stream.set_loop(true);
        stream.set_muted(true);
        let crop = VideoCrops::shared().and_then(|crops| crops.get(&path).ok().flatten());
        stream.set_framing(crate::mpv::Framing {
            crop: crop.is_some().then_some(crate::mpv::framing::CropMethod::CropDetect),
            ..Default::default()
        });
        if let Some(crop) = crop {
            stream.lock_crop(Some(crop));
        }
        stream.set_scale(f64::from(cell.scale_factor()));
        player.set_paintable(Some(&VideoPreview::new(&stream, self.thumbnail_crop())));
        player.set_visible(true);
        if let Some(icon) = icon_of(cell) {
            icon.set_visible(false);
        }
        stream.play();
        self.imp().embedded.replace(Some(Embedded {
            cell: cell.downgrade(),
            path,
            stream,
        }));
    }

    /// Stops the embedded preview, if one is playing or about to, and shows the still thumbnail again.
    fn stop_embedded(&self) {
        if let Some(timer) = self.imp().embedded_timer.take() {
            timer.remove();
        }
        self.imp().pending_embedded.take();
        let Some(embedded) = self.imp().embedded.take() else { return };
        embedded.stream.stop();
        let Some(cell) = embedded.cell.upgrade() else { return };
        if let Some(player) = player_of(&cell) {
            player.set_paintable(None::<&gtk::gdk::Paintable>);
            player.set_visible(false);
        }
        if let Some(icon) = icon_of(&cell) {
            icon.set_visible(true);
        }
    }

    /// Stops the embedded preview if it is playing, or about to play, in `cell`, as when the cell stops showing its item.
    fn stop_embedded_in(&self, cell: &gtk::Box) {
        let ours = self.imp().embedded.borrow().as_ref().and_then(|embedded| embedded.cell.upgrade());
        let waiting = self.imp().pending_embedded.borrow().as_ref().and_then(|(waiting_cell, _)| waiting_cell.upgrade());
        if ours.as_ref() == Some(cell) || waiting.as_ref() == Some(cell) {
            self.stop_embedded();
        }
    }

    /// Rebinds every item so a size or layout change shows, keeping the item at the top of the view
    /// where it was.
    fn refresh_items(&self) {
        let imp = self.imp();
        self.stop_hover();
        self.stop_embedded();
        let anchor = top_item_anchor(&imp.grid);
        imp.grid.set_cell_size(self.item_size(), self.cell_height());
        imp.grid.refresh();
        self.restart_prefetch();
        if let Some(anchor) = anchor {
            restore_top_item_after_layout(&imp.grid, anchor);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;

    /// Pumps the main context until `cond` holds, failing after five seconds.
    fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for: {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// Runs the main context for about `frames` frames, so layout and item binding happen.
    fn pump_frames(frames: u32) {
        let ctx = glib::MainContext::default();
        for _ in 0..frames {
            while ctx.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    thread_local! {
        /// How many times a folder's previews have been laid out again.
        pub(super) static RELAYOUTS: Cell<usize> = const { Cell::new(0) };
    }

    /// Each picture's metadata lays a fitted folder out again once, rather than each layout asking again and so
    /// doubling the layouts with every picture.
    fn a_fitted_folder_is_laid_out_once_per_picture() {
        const PICTURES: usize = 12;
        let root = std::env::temp_dir().join(format!("rsimgv-browse-fit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("album");
        fs::create_dir_all(&folder).unwrap();
        let pictures: Vec<PathBuf> = (0..PICTURES).map(|index| folder.join(format!("{index:02}.png"))).collect();
        for path in &pictures {
            let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 64, 32).unwrap();
            pixbuf.fill(0x3366ccff);
            pixbuf.savev(path, "png", &[]).unwrap();
        }

        RELAYOUTS.with(|count| count.set(0));
        let view = BrowseView::new();
        view.set_directory_previews(true);
        view.set_folder_preview_fit(true);
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 1);
        let info = MediaInfo::shared();
        wait_for("every picture's metadata", || {
            pictures.iter().all(|path| {
                let size = fs::metadata(path).unwrap().len();
                info.get(path, crate::fs::thumbnail::stamp(path), size).is_some()
            })
        });
        pump_frames(30);

        // The item may be bound more than once while the window settles, and each bind asks for each picture once.
        let relayouts = RELAYOUTS.with(Cell::get);
        assert!((1..=2 * PICTURES).contains(&relayouts), "laid out again {relayouts} times");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// The visual box (the icon's frame) of every rendered item.
    fn rendered_visuals(view: &BrowseView) -> Vec<gtk::Overlay> {
        let mut visuals = Vec::new();
        let mut child = view.imp().grid.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(visual) = widget.first_child().and_downcast::<gtk::Overlay>() {
                visuals.push(visual);
            }
        }
        visuals
    }

    /// Changing the size updates items that are already on screen, not just the next ones bound.
    fn item_size_change_reaches_rendered_items() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-size-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("two")).unwrap();
        fs::write(root.join("three.mp4"), "x").unwrap();

        let view = BrowseView::new();
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 3);
        pump_frames(60);

        view.set_item_size(128);
        pump_frames(60);
        let visuals = rendered_visuals(&view);
        assert!(!visuals.is_empty(), "items are rendered");
        assert!(visuals.iter().all(|v| v.size_request() == (128, 128)), "128px before the change");

        view.set_item_size(64);
        pump_frames(60);
        let visuals = rendered_visuals(&view);
        assert!(visuals.iter().all(|v| v.size_request() == (64, 64)), "64px after the change");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// Changing the selected file on disk brings the listing up to date without the selection being reported as
    /// lost: the file is replaced in the listing and selected again, and no empty selection is reported on the way.
    fn modified_selection_is_not_reported_as_lost() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-reconcile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            fs::write(root.join(name), "x").unwrap();
        }

        let view = BrowseView::new();
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 3);
        view.select_named("b.png");
        // Watching the folder is what brings the change in; a view that is not active does not watch it.
        view.set_active(true);
        pump_frames(30);

        let reported = Rc::new(RefCell::new(Vec::new()));
        let reported_clone = reported.clone();
        view.connect_selection_changed(move |_, name| reported_clone.borrow_mut().push(name.to_owned()));
        fs::write(root.join("b.png"), "changed contents").unwrap();
        let listed_size = || {
            let store = view.imp().store.get()?;
            store.iter::<FileEntry>().filter_map(Result::ok).find(|entry| entry.name() == "b.png").map(|entry| entry.size())
        };
        wait_for("the change is listed", || listed_size() == Some(16));
        pump_frames(30);

        assert!(!reported.borrow().iter().any(String::is_empty), "no empty selection is reported: {:?}", reported.borrow());
        assert_eq!(view.selected_name().as_deref(), Some("b.png"), "the modified file is selected again");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// A thumbnail is let go from memory once its item has left the screen, by its cell too.
    fn an_item_leaving_the_screen_lets_go_of_its_thumbnail() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-unload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (pictures, empty) = (root.join("pictures"), root.join("empty"));
        fs::create_dir_all(&pictures).unwrap();
        fs::create_dir_all(&empty).unwrap();
        let picture = pictures.join("picture.png");
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 64, 32).unwrap();
        pixbuf.fill(0x3366ccff);
        pixbuf.savev(&picture, "png", &[]).unwrap();

        let view = BrowseView::new();
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&pictures);
        let thumbnails = Thumbnails::shared();
        wait_for("the thumbnail", || thumbnails.has_loaded(&picture));
        let cell = view.imp().grid.first_child().and_downcast::<gtk::Box>().expect("a cell");
        let icon = icon_of(&cell).expect("an icon");
        wait_for("the thumbnail on its item", || icon.paintable().is_some());

        view.set_path(&empty);
        wait_for("the empty folder", || view.item_count() == 0);
        wait_for("the thumbnail let go", || !thumbnails.has_loaded(&picture));
        assert_eq!(icon.storage_type(), gtk::ImageType::Empty, "the spare cell does not keep it");

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// Changing whether previews are fitted reaches the folders already on screen.
    fn the_fit_setting_reaches_folders_on_screen() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-fit-setting-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("album");
        fs::create_dir_all(&folder).unwrap();
        for index in 0..12 {
            // Wide pictures cover less of the item, so more of them fit than the set number.
            let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 64, 32).unwrap();
            pixbuf.fill(0x3366ccff);
            pixbuf.savev(folder.join(format!("{index:02}.png")), "png", &[]).unwrap();
        }

        let view = BrowseView::new();
        view.set_directory_previews(true);
        view.set_folder_preview_count(4);
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 1);
        let shown = || {
            let cell = view.imp().grid.first_child().and_downcast::<gtk::Box>()?;
            scattered_in(&cell).map(|scattered| scattered.shown())
        };
        wait_for("the set number of previews", || shown() == Some(4));

        view.set_folder_preview_fit(true);
        wait_for("as many previews as fit", || shown() == Some(9));
        view.set_folder_preview_fit(false);
        wait_for("the set number again", || shown() == Some(4));

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    /// A folder's previews are up once it is scanned, before any picture is made, so metadata that comes in first
    /// can lay them out. These pictures can never be made, so a folder waiting for its first would wait for good.
    fn a_folder_s_previews_are_up_before_its_first_picture() {
        let root = std::env::temp_dir().join(format!("rsimgv-browse-early-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("album");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("broken.png"), "not really a picture").unwrap();

        let view = BrowseView::new();
        view.set_directory_previews(true);
        let window = gtk::Window::new();
        window.set_default_size(600, 400);
        window.set_child(Some(&view));
        window.present();
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 1);
        wait_for("the previews", || {
            view.imp().grid.first_child().and_downcast::<gtk::Box>().is_some_and(|cell| scattered_in(&cell).is_some())
        });

        window.destroy();
        let _ = fs::remove_dir_all(&root);
    }

    pub(crate) fn run_scenarios() {
        item_size_change_reaches_rendered_items();
        modified_selection_is_not_reported_as_lost();
        a_fitted_folder_is_laid_out_once_per_picture();
        the_fit_setting_reaches_folders_on_screen();
        an_item_leaving_the_screen_lets_go_of_its_thumbnail();
        a_folder_s_previews_are_up_before_its_first_picture();
        let root = std::env::temp_dir().join(format!("rsimgv-browse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::write(root.join("b.png"), "not really a picture").unwrap();
        fs::write(root.join("a.mp4"), "not really a video").unwrap();

        let view = BrowseView::new();
        assert_eq!(view.item_size(), DEFAULT_ITEM_SIZE);
        view.set_path(&root);
        wait_for("listing", || view.item_count() == 3);
        // Media only is the default, so the text file is left out.
        assert!(view.media_only());
        assert_eq!(view.item_names(), ["sub", "a.mp4", "b.png"]);

        view.set_media_only(false);
        assert_eq!(view.item_names(), ["sub", "a.mp4", "a.txt", "b.png"], "folders first, then natural order");
        view.set_show_hidden(true);
        assert_eq!(view.item_count(), 5);
        view.set_show_hidden(false);
        assert_eq!(view.item_count(), 4);
        view.set_media_only(true);
        assert_eq!(view.item_names(), ["sub", "a.mp4", "b.png"]);

        let opened = Rc::new(RefCell::new(Vec::new()));
        let sink = opened.clone();
        view.connect_directory_activated(move |_, path| sink.borrow_mut().push(path.to_path_buf()));
        view.activate(0);
        assert_eq!(*opened.borrow(), vec![root.join("sub")]);

        // Resizing and re-laying out the items does not lose them.
        view.set_item_size(64);
        view.set_filename_display("wrapped".to_owned());
        view.set_info_fields(&[InfoField::Size]);
        assert_eq!(view.item_count(), 3);

        let _ = fs::remove_dir_all(&root);
    }
}

/// Scrolls to `value` once the newly listed items have been laid out, over a few frames because the
/// grid's height grows as it measures them.
fn restore_scroll_after_layout(view: &BrowseView, scroller: &gtk::ScrolledWindow, value: f64) {
    const FRAMES: u32 = 8;
    let frames = Cell::new(0u32);
    view.imp().restoring_scroll.set(true);
    let view = view.downgrade();
    scroller.add_tick_callback(move |scroller, _| {
        frames.set(frames.get() + 1);
        // Stops once the user has selected an item, since the selection brings its item into view.
        let Some(view) = view.upgrade() else { return glib::ControlFlow::Break };
        if !view.imp().restoring_scroll.get() {
            return glib::ControlFlow::Break;
        }
        scroller.vadjustment().set_value(value);
        if frames.get() < FRAMES {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    fn waiter(wanted: bool) -> ScanWaiter {
        ScanWaiter {
            wanted: Box::new(move || wanted),
            on_ready: Box::new(|_| {}),
        }
    }

    fn folder(index: usize) -> PathBuf {
        PathBuf::from(format!("/folder{index}"))
    }

    #[test]
    fn only_a_few_scans_run_at_once_and_the_newest_goes_first() {
        let mut scans = FolderScans::default();
        for index in 0..=MAX_SCANS {
            scans.ask((folder(index), 1), waiter(true));
        }
        assert_eq!(scans.start_next(), Some((folder(MAX_SCANS), 1)));
        for _ in 1..MAX_SCANS {
            assert!(scans.start_next().is_some());
        }
        assert_eq!(scans.start_next(), None, "no room");

        scans.finish((folder(MAX_SCANS), 1), Rc::default());
        assert_eq!(scans.start_next(), Some((folder(0), 1)), "room again");
    }

    #[test]
    fn a_scan_asked_for_twice_is_made_once() {
        let mut scans = FolderScans::default();
        let key = (folder(0), 1);
        scans.ask(key.clone(), waiter(true));
        scans.ask(key.clone(), waiter(true));
        assert_eq!(scans.start_next(), Some(key.clone()));
        assert_eq!(scans.start_next(), None);
        assert_eq!(scans.finish(key, Rc::default()).len(), 2);
    }

    #[test]
    fn a_scan_nothing_wants_is_dropped() {
        let mut scans = FolderScans::default();
        let (wanted, scrolled_away) = ((folder(0), 1), (folder(1), 1));
        scans.ask(wanted.clone(), waiter(true));
        scans.ask(scrolled_away.clone(), waiter(false));
        assert_eq!(scans.start_next(), Some(wanted));
        assert!(!scans.waiters.contains_key(&scrolled_away));
    }

    #[test]
    fn the_least_recently_used_scan_goes_first() {
        let mut scans = FolderScans::default();
        for index in 0..MAX_CACHED_SCANS {
            scans.keep(folder(index), 1, Rc::default());
        }
        assert!(scans.get(&folder(0), 1).is_some());

        scans.keep(folder(MAX_CACHED_SCANS), 1, Rc::default());

        assert_eq!(scans.kept.len(), MAX_CACHED_SCANS);
        assert!(scans.get(&folder(0), 1).is_some(), "used just now");
        assert!(scans.get(&folder(1), 1).is_none(), "the oldest");
    }

    #[test]
    fn a_folder_s_first_names_are_taken_whatever_order_it_lists_them_in() {
        let names = |order: Vec<usize>| order.into_iter().map(|index| PathBuf::from(format!("{index:06}")));
        let forward = first_by_name(names((0..SCATTER_SCAN + 10).collect()));
        let backward = first_by_name(names((0..SCATTER_SCAN + 10).rev().collect()));
        assert_eq!(forward.len(), SCATTER_SCAN);
        assert_eq!(forward, backward);
        assert_eq!(forward[0], PathBuf::from("000000"));
    }

    #[test]
    fn a_huge_folder_is_read_only_so_far() {
        let mut listed = (0..SCATTER_READ + 5).map(|index| PathBuf::from(format!("{index:06}")));
        first_by_name(listed.by_ref());
        assert_eq!(listed.count(), 5);
    }

    #[test]
    fn a_folder_s_pictures_are_requested_at_the_size_they_are_drawn() {
        assert_eq!(photo_size(256, 1).pixels(), 128);
        assert_eq!(photo_size(256, 2).pixels(), 256);
        assert_eq!(photo_size(1024, 2).pixels(), 1024);
        assert_eq!(photo_size(32, 1).pixels(), 32);
    }

    #[test]
    fn a_kept_scan_is_for_the_folder_as_it_was() {
        let mut scans = FolderScans::default();
        scans.keep(folder(0), 1, Rc::default());
        assert!(scans.get(&folder(0), 2).is_none());
    }

    #[test]
    fn video_preview_modes_are_named_by_the_setting() {
        assert_eq!(VideoPreviews::from_name("none"), VideoPreviews::None);
        assert_eq!(VideoPreviews::from_name("frames"), VideoPreviews::Frames);
        assert_eq!(VideoPreviews::from_name("embedded"), VideoPreviews::Embedded);
        assert_eq!(VideoPreviews::from_name("something else"), VideoPreviews::None);
    }
}

#[cfg(test)]
mod run_tests {
    use super::runs;

    #[test]
    fn neighbouring_removals_form_one_run() {
        assert_eq!(runs(&[9, 8, 7, 4, 2, 1]), vec![(7, 3), (4, 1), (1, 2)]);
        assert_eq!(runs(&[]), Vec::<(u32, u32)>::new());
    }
}
