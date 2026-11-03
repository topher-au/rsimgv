//! The media viewer: one picture or video filling the window on black, under a header bar with the Back button,
//! the file name and its place in the folder, zoom and fit buttons, fullscreen, and a pin. In fullscreen the
//! header floats over the picture and shows only while the pointer is near the top, unless pinned. A video's control bar floats
//! along the bottom and shows only while the pointer is near it. Left and right step through the pictures
//! and videos of the folder, Escape goes back, and space plays or pauses a video; these keys are bindings,
//! see `keybindings`.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::drag;
use super::image_canvas::{Fit, ImageCanvas, OriginalFocus, Placement, Scaling};
use crate::file_settings::FileSettings;
use crate::fs::thumbnail::Thumbnails;
use crate::orientation::Orientation;
use crate::fs::metadata::MediaInfo;
use crate::fs::thumbnail::Pixels;
use super::media_controls::MediaControls;
use super::overlay_split_view::OverlaySplitView;
use super::backdrop::Backdrop;
use super::media_overlay::{clock, fraction, Frame, MediaOverlay};
use crate::overlay::{Layout, Trigger, TRIGGERS};
use super::thumbnail_bar::ThumbnailBar;

const MICROS: f64 = 1_000_000.0;

/// When the zoom level is shown: always, never, or only once the user has zoomed the picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ZoomIndicator {
    On,
    Off,
    #[default]
    Auto,
}

impl ZoomIndicator {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "on" => Some(ZoomIndicator::On),
            "off" => Some(ZoomIndicator::Off),
            "auto" => Some(ZoomIndicator::Auto),
            _ => None,
        }
    }
}

/// The item the viewer shows: its path, its size in pixels, and for a video, its playback.
pub struct ShownItem {
    pub path: PathBuf,
    pub width: i32,
    pub height: i32,
    pub playback: Option<Playback>,
}

/// A video's playback, in seconds and frames per second. A value is `None` when it is not known.
pub struct Playback {
    pub position: f64,
    pub duration: Option<f64>,
    pub fps: Option<f64>,
    pub speed: Option<f64>,
}

/// How much a volume key changes the volume, as a fraction of full.
pub const VOLUME_STEP: f64 = 0.05;

/// The loudest the volume goes, as a fraction of full: the scale runs to 200%.
pub const MAX_VOLUME: f64 = 2.0;

/// The share of the viewer's height, from its bottom edge, where a video's controls appear.
const REVEAL_FRACTION: f64 = 0.2;

/// The space kept between the video controls and the edge of the frame, and how far from their left and right edges they
/// are grabbed to change their width, in pixels.
const CONTROLS_EDGE: f64 = 12.0;
const CONTROLS_GRAB: f64 = 14.0;
/// How far around the controls the pointer brings them up, in pixels.
const CONTROLS_REVEAL_MARGIN: f64 = 48.0;
/// The narrowest the controls are made by dragging, as a share of the frame's width.
const CONTROLS_MIN_WIDTH: f64 = 0.2;

/// Where the video controls sit over the frame, and how wide they are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlsLayout {
    /// Whether the controls stay where they are, rather than being dragged.
    pub locked: bool,
    /// The centre of the controls, as a share of the frame's width and height.
    pub x: f64,
    pub y: f64,
    /// Their width as a share of the frame's width, or 0 for the width they need.
    pub width: f64,
}

impl Default for ControlsLayout {
    fn default() -> Self {
        Self { locked: true, x: 0.5, y: 0.92, width: 0.0 }
    }
}

/// How wide the controls are in a frame `frame_width` wide, when they need `natural` pixels.
fn controls_width(frame_width: f64, natural: f64, layout: &ControlsLayout) -> f64 {
    let room = (frame_width - 2.0 * CONTROLS_EDGE).max(1.0);
    let width = if layout.width > 0.0 { frame_width * layout.width } else { natural };
    width.min(room)
}

/// The left and top of controls `size` big in a frame `frame` big, centred where the layout says and kept within the frame.
fn controls_origin(frame: (f64, f64), size: (f64, f64), layout: &ControlsLayout) -> (f64, f64) {
    let place = |frame: f64, size: f64, centre: f64| {
        let most = (frame - size - CONTROLS_EDGE).max(CONTROLS_EDGE);
        (frame * centre - size / 2.0).clamp(CONTROLS_EDGE, most)
    };
    (place(frame.0, size.0, layout.x), place(frame.1, size.1, layout.y))
}

/// What dragging the controls changes.
#[derive(Debug, Clone, Copy)]
enum ControlsDrag {
    Move,
    ResizeLeft,
    ResizeRight,
}

/// How near, in seconds, a video's start or end its position must be to start it from the beginning next time.
const POSITION_MARGIN: f64 = 2.0;

/// How long after a skip the overlay shows where it went, rather than where the video is, which lags behind it.
const SEEK_SETTLE: std::time::Duration = std::time::Duration::from_millis(400);
/// How long the playback speed has to stay put before SVP is rebuilt for it, as each rebuild holds up the video.
const SVP_SPEED_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);
/// How long, in milliseconds, a video waits to start by default after the viewer steps to it.
const DEFAULT_VIDEO_START_DELAY: u64 = 150;

/// The pictures decoded ahead of the item shown, at most this many.
const PREFETCHED: usize = 2;

/// Decodes the picture at `path` on a worker thread, so the window stays responsive. `None` if it cannot be read.
async fn decode_picture(path: PathBuf) -> Option<Pixels> {
    let work = path.clone();
    let decoded = gio::spawn_blocking(move || Pixels::from_file(&work)).await.ok()?;
    match decoded {
        Ok(pixels) => Some(pixels),
        Err(err) => {
            eprintln!("rsimgv: could not open {}: {err}", path.display());
            None
        }
    }
}

/// When the file with metadata `file` was last modified, in seconds since the epoch, or 0 if it is unknown.
fn mtime_secs(file: &std::fs::Metadata) -> i64 {
    file.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_secs() as i64)
}

/// Whether `path` is a video, judged by its content type.
pub fn is_video(path: &Path) -> bool {
    gio::content_type_guess(Some(path), None::<&[u8]>).0.starts_with("video/")
}

/// How near, in pixels, the pointer must come to the window's edge to show the thumbnail bar.
const BAR_EDGE_ZONE: f64 = 8.0;

/// How far, in pixels, past the shown thumbnail bar's inner edge the pointer can go before it hides.
const BAR_BUFFER: f64 = 32.0;

/// How long, in milliseconds, the thumbnail bar waits after the pointer moves away before it hides.
const BAR_HIDE_MS: u64 = 2000;

/// How long, in milliseconds, the thumbnail bar slides in or out.
const BAR_SLIDE_MS: u32 = 250;

/// How long, in milliseconds, the thumbnail bar stays shown when it is turned on.
const BAR_PEEK_MS: u64 = 1500;
/// How long the pointer stays still before it is hidden, when the cursor is set to hide.
const CURSOR_HIDE_MS: u64 = 1500;

/// Which of a file's settings the viewer keeps for it and applies when the file is shown again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remember {
    /// Whether anything is kept for a file at all.
    pub enabled: bool,
    pub position: bool,
    pub speed: bool,
    pub volume: bool,
    pub rotation: bool,
    pub zoom: bool,
    /// Where a zoomed picture is panned to, which only means something with the zoom it goes with.
    pub pan: bool,
}

impl Default for Remember {
    fn default() -> Self {
        Self { enabled: true, position: true, speed: true, volume: false, rotation: false, zoom: true, pan: true }
    }
}

impl Remember {
    /// Whether the file setting `key` is kept.
    fn keeps(&self, key: &str) -> bool {
        self.enabled
            && match key {
                "position" => self.position,
                "speed" => self.speed,
                "volume" | "muted" => self.volume,
                "rotation" | "mirrored" => self.rotation,
                "fit" | "zoom" => self.zoom,
                "pan-x" | "pan-y" => self.zoom && self.pan,
                _ => true,
            }
    }
}

/// The settings changes that save how `orientation` turns and mirrors what is shown.
fn orientation_settings(orientation: Orientation) -> Vec<(&'static str, Option<serde_yaml_ng::Value>)> {
    vec![
        ("rotation", (orientation.degrees() != 0).then(|| orientation.degrees().into())),
        ("mirrored", orientation.is_mirrored().then(|| true.into())),
    ]
}

/// The orientation saved in `settings`, if there is one.
fn saved_orientation(settings: &FileSettings) -> Option<Orientation> {
    let (degrees, mirrored) = (settings.get_f64("rotation"), settings.get_bool("mirrored"));
    (degrees.is_some() || mirrored.is_some()).then(|| Orientation::from_parts(mirrored.unwrap_or(false), degrees.unwrap_or(0.0)))
}

/// The settings changes that keep where `media` was left: its position, unless it is near its start or end, and
/// its speed, unless it is normal.
fn video_settings(media: &gtk::MediaStream) -> Vec<(&'static str, Option<serde_yaml_ng::Value>)> {
    let position = media.timestamp() as f64 / 1_000_000.0;
    let duration = media.duration() as f64 / 1_000_000.0;
    let mid_way = media.is_prepared()
        && !media.is_ended()
        && position > POSITION_MARGIN
        && (duration <= 0.0 || position < duration - POSITION_MARGIN);
    let speed = media.downcast_ref::<crate::mpv::MpvStream>().and_then(crate::mpv::MpvStream::speed);
    let mut changes = vec![("position", mid_way.then(|| position.into()))];
    // Only the mpv player knows its speed; with GTK's, the saved speed is left as it is.
    if let Some(speed) = speed {
        changes.push(("speed", ((speed - 1.0).abs() > 1e-6).then(|| speed.into())));
    }
    changes
}

/// The edge of a `width` by `height` area nearest the point `x`, `y`, measured as a share of the area,
/// so a wide area does not favour the top and bottom.
pub fn nearest_side(x: f64, y: f64, width: i32, height: i32) -> gtk::PositionType {
    let (width, height) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let distances = [
        (x / width, gtk::PositionType::Left),
        ((width - x) / width, gtk::PositionType::Right),
        (y / height, gtk::PositionType::Top),
        ((height - y) / height, gtk::PositionType::Bottom),
    ];
    distances.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map_or(gtk::PositionType::Bottom, |(_, side)| side)
}

/// The setting value for a side of the viewer.
pub fn side_name(side: gtk::PositionType) -> &'static str {
    match side {
        gtk::PositionType::Top => "top",
        gtk::PositionType::Left => "left",
        gtk::PositionType::Right => "right",
        _ => "bottom",
    }
}

/// The side of the viewer a setting value names.
pub fn side_from_name(name: &str) -> gtk::PositionType {
    match name {
        "top" => gtk::PositionType::Top,
        "left" => gtk::PositionType::Left,
        "right" => gtk::PositionType::Right,
        _ => gtk::PositionType::Bottom,
    }
}

/// The index of the item before or after `index` in a list of `len` items, if there is one.
pub fn neighbour(index: usize, len: usize, forward: bool) -> Option<usize> {
    if forward { (index + 1 < len).then_some(index + 1) } else { index.checked_sub(1) }
}

/// What stepping past the first or last item does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FolderEnd {
    #[default]
    Stop,
    Loop,
    NextFolder,
}

impl FolderEnd {
    /// The behaviour a setting value names, or the default for any other.
    pub fn from_name(name: &str) -> Self {
        match name {
            "loop" => Self::Loop,
            "next-folder" => Self::NextFolder,
            _ => Self::Stop,
        }
    }
}

/// The item a step moves to in the folder: its neighbour or, when looping, the item at the other end.
fn step_index(index: usize, len: usize, forward: bool, end: FolderEnd) -> Option<usize> {
    neighbour(index, len, forward).or_else(|| (end == FolderEnd::Loop && len > 1).then(|| if forward { 0 } else { len - 1 }))
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/media_viewer.ui")]
    pub struct MediaViewer {
        #[template_child]
        pub pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub canvas: TemplateChild<ImageCanvas>,
        #[template_child]
        pub back_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub filename: TemplateChild<gtk::Label>,
        #[template_child]
        pub position: TemplateChild<gtk::Label>,
        #[template_child]
        pub header_dock: TemplateChild<gtk::Box>,
        #[template_child]
        pub body: TemplateChild<gtk::Box>,
        /// The body and the status bar under it, which the playlist sidebar sits beside.
        #[template_child]
        pub content_column: TemplateChild<gtk::Box>,
        #[template_child]
        pub playlist_split: TemplateChild<OverlaySplitView>,
        #[template_child]
        pub pin_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub zoom_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub zoom_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub header: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub controls_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub controls: TemplateChild<MediaControls>,
        #[template_child]
        pub overlay: TemplateChild<gtk::Overlay>,
        /// The picture over the status bar, beside a thumbnail bar that sits at the left or right.
        pub(super) picture_column: gtk::Box,
        #[template_child]
        pub status_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub status_counter: TemplateChild<gtk::Label>,
        #[template_child]
        pub status_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub status_resolution: TemplateChild<gtk::Label>,
        #[template_child]
        pub status_expander: TemplateChild<gtk::Box>,
        #[template_child]
        pub status_fps: TemplateChild<gtk::Label>,
        #[template_child]
        pub status_size: TemplateChild<gtk::Label>,
        /// The thumbnails of the items, floating over an edge of the picture.
        pub(super) thumbnail_bar: ThumbnailBar,
        /// Slides the thumbnail bar in from its edge and out again.
        pub(super) bar_revealer: gtk::Revealer,
        /// The side of the picture the bar is on (bottom for `None`), and whether it is turned on.
        pub(super) bar_side: Cell<Option<gtk::PositionType>>,
        pub(super) bar_shown: Cell<bool>,
        /// Whether the bar floats over the picture rather than sitting beside it, and whether, floating, it
        /// stays shown rather than only while the pointer is near.
        pub(super) bar_overlay: Cell<bool>,
        pub(super) bar_pinned: Cell<bool>,
        /// Hides the bar again after it was shown briefly on being turned on.
        pub(super) bar_peek: RefCell<Option<glib::SourceId>>,
        /// Hides the bar once the pointer has been away from it for a while.
        pub(super) bar_hide: RefCell<Option<glib::SourceId>>,
        /// Where the pointer last was in the viewer, or `None` once it left.
        pub(super) pointer: Cell<Option<(f64, f64)>>,
        /// The folder's pictures and videos, in display order, and the index of the one shown.
        pub(super) paths: RefCell<Vec<PathBuf>>,
        pub(super) index: Cell<usize>,
        /// Whether the pictures either side of the item shown are decoded ahead of time.
        pub(super) preloading: Cell<bool>,
        /// Pictures decoded ahead of the item shown, with the paths they are of.
        pub(super) prefetched: RefCell<Vec<(PathBuf, Pixels)>>,
        /// The settings kept for the item shown: a picture's zoom and pan, a video's position and speed. They are
        /// saved when it is left.
        pub(super) file_settings: RefCell<Option<FileSettings>>,
        /// Which of a file's settings are kept, and the volume and mute state the video started with, so a change to them
        /// can be told from the level a video merely inherited.
        pub(super) remember: Cell<Remember>,
        /// How videos played by mpv are framed.
        pub(super) framing: Cell<crate::mpv::Framing>,
        pub(super) audio_at_load: Cell<(f64, bool)>,
        /// The volume and mute state the user had before any video's own was applied, which the videos without their own start at.
        pub(super) audio_base: Cell<Option<(f64, bool)>>,
        /// The video shown, if the item is one. The viewer owns it, and the controls drive it.
        pub(super) media: RefCell<Option<gtk::MediaStream>>,
        /// Whether a video starts again when it ends, and whether the next item plays when it ends instead.
        pub(super) repeat: Cell<bool>,
        pub(super) auto_advance: Cell<bool>,
        /// What stepping past the first or last item does.
        pub(super) folder_end: Cell<FolderEnd>,
        /// Whether videos are played by libmpv rather than GTK's own media backend.
        pub(super) use_mpv: Cell<bool>,
        /// Extra mpv command line options for the mpv player.
        pub(super) mpv_options: RefCell<String>,
        /// Whether the controls and the title stay shown, rather than hiding until the pointer comes near.
        pub(super) pinned: Cell<bool>,
        /// Whether the window is fullscreen, where the header floats over the picture and hides.
        pub(super) fullscreen: Cell<bool>,
        /// Hides the header once the pointer has been away from it for a while.
        pub(super) header_hide: RefCell<Option<glib::SourceId>>,
        /// Whether an item has been shown since the viewer was opened, so the next video follows another item.
        pub(super) shown_any: Cell<bool>,
        /// Whether the video being loaded waits before it starts, and the wait under way.
        pub(super) delay_video: Cell<bool>,
        /// Whether the next item shown was stepped to, rather than chosen, so a video there waits to start.
        pub(super) delay_next: Cell<bool>,
        /// How long a stepped-to video waits to start, in milliseconds.
        pub(super) video_start_delay: Cell<u64>,
        pub(super) video_start: RefCell<Option<glib::SourceId>>,
        /// Whether the zoom level is shown, and whether the pointer is hidden while it is still.
        pub(super) zoom_indicator: Cell<ZoomIndicator>,
        pub(super) auto_hide_cursor: Cell<bool>,
        /// Hides the pointer once it has been still over the viewer for a while.
        pub(super) cursor_hide: RefCell<Option<glib::SourceId>>,
        /// Whether the status bar under the item is shown.
        pub(super) status_shown: Cell<bool>,
        /// Where the video controls sit, and the drag that is moving them, with the layout and width it started from.
        pub(super) controls_layout: Cell<ControlsLayout>,
        pub(super) controls_drag: Cell<Option<(ControlsDrag, ControlsLayout, f64)>>,
        /// The overlay of elements over the item, made when it is first wanted.
        pub(super) media_overlay: OnceCell<MediaOverlay>,
        /// The page that shows a video: the blurred thumbnail behind, and the video over it.
        pub(super) video_page: OnceCell<gtk::Overlay>,
        pub(super) backdrop: Backdrop,
        /// The band that shows which edge the thumbnail bar would go to if it were dropped, while it is dragged.
        pub(super) drop_indicator: OnceCell<gtk::Box>,
        /// The surface whose scale the video's frames follow, and the handler that watches it.
        pub(super) scale_watch: RefCell<Option<(gdk::Surface, glib::SignalHandlerId)>>,
        /// The timer that keeps the overlay up to date while any element is on.
        pub(super) overlay_tick: RefCell<Option<glib::SourceId>>,
        /// Where the last skip went and when, so the overlay shows that while the video catches up with it.
        pub(super) seek_landing: Cell<Option<(i64, std::time::Instant)>>,
        /// Whether the last skip went forward.
        pub(super) seek_forward: Cell<bool>,
        /// When the video was last skipped through, for the elements that show for a while after a skip.
        pub(super) changed_at: Cell<[Option<std::time::Instant>; TRIGGERS]>,
        /// What each change an element waits for last read, to tell when it changes, and until when a picture's zoom is
        /// still settling after it is shown.
        pub(super) watched: Cell<[Option<f64>; TRIGGERS]>,
        pub(super) zoom_settles: Cell<Option<std::time::Instant>>,
        pub(super) zoom_scale: Cell<f64>,
        /// The SVP frame interpolation settings, applied to the videos mpv plays.
        pub(super) svp: RefCell<Option<crate::svp::Config>>,
        /// The filter that applies SVP to the video shown, while SVP is on for it.
        pub(super) svp_filter: RefCell<Option<String>>,
        /// What the notification says once that filter is in place.
        pub(super) svp_announcement: RefCell<Option<String>>,
        /// The timer that puts that filter in place.
        pub(super) svp_timer: RefCell<Option<glib::SourceId>>,
        /// Whether that filter is being kept out, as the video was skipped through or has only just started.
        pub(super) svp_held: Cell<bool>,
        /// This viewer's hold on the server SVP's libraries connect to, kept while SVP is on.
        pub(super) svp_manager: RefCell<Option<std::rc::Rc<crate::svp_manager::Manager>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaViewer {
        const NAME: &'static str = "RsimgvMediaViewer";
        type Type = super::MediaViewer;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            // The template references the controls, so their type must be registered first.
            MediaControls::static_type();
            ImageCanvas::static_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MediaViewer {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let weak = obj.downgrade();
            self.video_start_delay.set(DEFAULT_VIDEO_START_DELAY);
            // A video is shown over a blurred copy of its thumbnail, which fills the view until its frames are up.
            let video_page = gtk::Overlay::new();
            video_page.set_hexpand(true);
            video_page.set_vexpand(true);
            self.backdrop.set_hexpand(true);
            self.backdrop.set_vexpand(true);
            video_page.set_child(Some(&self.backdrop));
            self.pages.remove(&*self.picture);
            video_page.add_overlay(&*self.picture);
            self.pages.add_child(&video_page);
            let _ = self.video_page.set(video_page);
            // Frames follow the scale of the surface the picture is on, which changes when the window moves between displays.
            self.picture.connect_map({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.watch_display_scale();
                    }
                }
            });
            obj.setup_controls_placement();
            // The overlay is kept up to date only while the viewer is on screen.
            obj.connect_map(|viewer| {
                viewer.sync_overlay_tick();
                viewer.refresh_overlay();
            });
            obj.connect_unmap(|viewer| viewer.sync_overlay_tick());
            let weak_viewer = obj.downgrade();
            crate::ui::media_overlay::watch_preview(move || {
                if let Some(viewer) = weak_viewer.upgrade() {
                    viewer.sync_overlay_tick();
                    viewer.refresh_overlay();
                }
            });

            // The picture sits in a column of its own, so a thumbnail bar beside it can run the full height with the
            // status bar under the picture alone.
            self.picture_column.set_orientation(gtk::Orientation::Vertical);
            self.picture_column.set_hexpand(true);
            self.picture_column.set_vexpand(true);
            self.body.remove(&*self.overlay);
            self.picture_column.append(&*self.overlay);
            self.body.append(&self.picture_column);

            // Dragging the shown file carries it to other applications. A zoomed picture is panned by dragging
            // instead, so there is no file drag then.
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::COPY);
            drag.connect_prepare({
                let weak = weak.clone();
                move |_, _, _| {
                    let viewer = weak.upgrade()?;
                    let imp = viewer.imp();
                    let shows_canvas = imp.pages.visible_child().as_ref() == Some(imp.canvas.upcast_ref::<gtk::Widget>());
                    if shows_canvas && imp.canvas.is_zoomed() {
                        return None;
                    }
                    Some(drag::file_list(&[viewer.shown_path()?]))
                }
            });
            self.pages.add_controller(drag);

            // A file dropped on the viewer is shown, wherever it is.
            let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
            drop.connect_drop({
                let weak = weak.clone();
                move |_, value, _, _| {
                    let Some(viewer) = weak.upgrade() else { return false };
                    let Some(path) = drag::dropped_paths(value).into_iter().find(|path| path.is_file()) else { return false };
                    viewer.emit_by_name::<()>("file-dropped", &[&path.to_string_lossy().into_owned()]);
                    true
                }
            });
            obj.add_controller(drop);

            self.back_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.request_back();
                    }
                }
            });
            self.controls.connect_auto_advance_requested({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.toggle_auto_advance();
                    }
                }
            });
            self.controls.connect_previous_requested({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.step(false);
                    }
                }
            });
            self.controls.connect_next_requested({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.step(true);
                    }
                }
            });

            let motion = gtk::EventControllerMotion::new();
            motion.set_propagation_phase(gtk::PropagationPhase::Capture);
            motion.connect_motion({
                let weak = weak.clone();
                move |_, x, y| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.wake_cursor();
                        viewer.reveal_near(Some((x, y)));
                    }
                }
            });
            motion.connect_leave({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.reveal_near(None);
                    }
                }
            });
            obj.add_controller(motion);

            self.bar_revealer.set_transition_duration(BAR_SLIDE_MS);
            self.overlay.add_overlay(&self.bar_revealer);
            self.pin_button.connect_toggled({
                let weak = weak.clone();
                move |button| {
                    let Some(viewer) = weak.upgrade() else { return };
                    viewer.imp().pinned.set(button.is_active());
                    viewer.place_header();
                    viewer.reveal_near(viewer.imp().pointer.get());
                }
            });
            self.canvas.connect_scale_changed({
                let weak = weak.clone();
                move |_, scale| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.imp().zoom_scale.set(scale);
                        viewer.imp().zoom_label.set_label(&format!("{:.0}%", scale * 100.0));
                        viewer.update_zoom_indicator();
                    }
                }
            });
            let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            indicator.add_css_class("thumbnail-drop-indicator");
            indicator.set_can_target(false);
            indicator.set_visible(false);
            self.overlay.add_overlay(&indicator);
            let _ = self.drop_indicator.set(indicator);
            self.thumbnail_bar.connect_side_hovered({
                let weak = weak.clone();
                move |bar, x, y| {
                    let Some(viewer) = weak.upgrade() else { return };
                    let Some(point) = bar.compute_point(&viewer, &gtk::graphene::Point::new(x as f32, y as f32)) else { return };
                    let side = nearest_side(f64::from(point.x()), f64::from(point.y()), viewer.width(), viewer.height());
                    viewer.show_drop_indicator(Some(side));
                }
            });
            self.thumbnail_bar.connect_side_hover_ended({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.show_drop_indicator(None);
                    }
                }
            });
            self.thumbnail_bar.connect_side_dropped({
                let weak = weak.clone();
                move |bar, x, y| {
                    let Some(viewer) = weak.upgrade() else { return };
                    let Some(point) = bar.compute_point(&viewer, &gtk::graphene::Point::new(x as f32, y as f32)) else {
                        return;
                    };
                    let side = nearest_side(f64::from(point.x()), f64::from(point.y()), viewer.width(), viewer.height());
                    viewer.emit_by_name::<()>("thumbnail-side-requested", &[&side_name(side)]);
                }
            });
            self.thumbnail_bar.connect_size_chosen({
                let weak = weak.clone();
                move |_, size| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.emit_by_name::<()>("thumbnail-size-requested", &[&size]);
                    }
                }
            });
            self.thumbnail_bar.connect_item_activated({
                let weak = weak.clone();
                move |_, index| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.load(index);
                    }
                }
            });
            obj.set_thumbnail_bar(gtk::PositionType::Bottom, false, true, false);

        }

        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder("back-requested").build(), Signal::builder("item-changed").param_types([String::static_type()]).build(), Signal::builder("file-dropped").param_types([String::static_type()]).build(), Signal::builder("thumbnail-side-requested").param_types([String::static_type()]).build(), Signal::builder("thumbnail-size-requested").param_types([i32::static_type()]).build(), Signal::builder("folder-end-reached").param_types([bool::static_type()]).build(), Signal::builder("notification-requested").param_types([String::static_type(), bool::static_type()]).build(), Signal::builder("controls-layout-changed").param_types([f64::static_type(), f64::static_type(), f64::static_type()]).build()]);
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for MediaViewer {}
    impl BoxImpl for MediaViewer {}
}

glib::wrapper! {
    pub struct MediaViewer(ObjectSubclass<imp::MediaViewer>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MediaViewer {
    /// The split view around the picture, whose sidebar holds the playlist while the viewer is shown.
    pub fn playlist_split(&self) -> OverlaySplitView {
        self.imp().playlist_split.clone()
    }

    /// The overlay over the picture, which the keybindings list sits over while the viewer is open.
    pub fn overlay(&self) -> gtk::Overlay {
        (*self.imp().overlay).clone()
    }

    /// Shows the item at `index` of `paths`, the folder's pictures and videos in display order.
    pub fn show(&self, paths: Vec<PathBuf>, index: usize) {
        self.imp().thumbnail_bar.set_paths(&paths);
        self.imp().paths.replace(paths);
        self.imp().prefetched.borrow_mut().clear();
        self.load(index);
    }

    /// Puts the thumbnail bar on the `side` of the picture, turned on or off. Floating (`overlay`), it lies over
    /// the picture and slides in only while the pointer is near that edge, unless `pinned`; otherwise it sits
    /// beside the picture. Turning it on shows it briefly.
    pub fn set_thumbnail_bar(&self, side: gtk::PositionType, shown: bool, overlay: bool, pinned: bool) {
        let imp = self.imp();
        let turned_on = shown && !imp.bar_shown.get();
        imp.bar_side.set(Some(side));
        imp.bar_shown.set(shown);
        imp.bar_overlay.set(overlay);
        imp.bar_pinned.set(pinned);
        self.place_status_bar();
        let bar = &imp.thumbnail_bar;
        let revealer = &imp.bar_revealer;
        bar.set_side(side);
        bar.set_toggles(pinned, overlay);

        // Take the bar out of wherever it is, then put it where it now goes.
        let body = &*imp.body;
        if bar.parent().as_ref() == Some(body.upcast_ref::<gtk::Widget>()) {
            body.remove(bar);
        } else if revealer.child().is_some() {
            revealer.set_child(None::<&gtk::Widget>);
        }
        revealer.set_visible(shown && overlay);
        bar.set_visible(shown);

        if !overlay {
            // The viewer box runs across the bar: a column with the bar above or below, a row beside.
            let horizontal = matches!(side, gtk::PositionType::Top | gtk::PositionType::Bottom);
            body.set_orientation(if horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
            if matches!(side, gtk::PositionType::Top | gtk::PositionType::Left) {
                body.prepend(bar);
            } else {
                body.append(bar);
            }
            return;
        }

        let (halign, valign, transition) = match side {
            gtk::PositionType::Top => (gtk::Align::Fill, gtk::Align::Start, gtk::RevealerTransitionType::SlideDown),
            gtk::PositionType::Bottom => (gtk::Align::Fill, gtk::Align::End, gtk::RevealerTransitionType::SlideUp),
            gtk::PositionType::Left => (gtk::Align::Start, gtk::Align::Fill, gtk::RevealerTransitionType::SlideRight),
            _ => (gtk::Align::End, gtk::Align::Fill, gtk::RevealerTransitionType::SlideLeft),
        };
        revealer.set_halign(halign);
        revealer.set_valign(valign);
        revealer.set_transition_type(transition);
        revealer.set_child(Some(bar));
        if turned_on && !pinned {
            self.peek_thumbnail_bar();
        } else {
            self.reveal_near(imp.pointer.get());
        }
    }

    /// The thumbnail bar, so its toggles can be followed.
    pub fn thumbnail_bar(&self) -> &ThumbnailBar {
        &self.imp().thumbnail_bar
    }

    /// Sets whether the pictures either side of the item shown are decoded ahead of time. Those already decoded are
    /// let go when it is turned off; when it is turned on, it starts with the next item shown.
    pub fn set_preloading(&self, preloading: bool) {
        let imp = self.imp();
        imp.preloading.set(preloading);
        if !preloading {
            imp.prefetched.borrow_mut().clear();
        }
    }

    /// Makes the thumbnails `size` pixels.
    pub fn set_thumbnail_size(&self, size: i32) {
        self.imp().thumbnail_bar.set_thumbnail_size(size);
    }

    /// Shows the thumbnail bar for a moment, then leaves it to the pointer.
    fn peek_thumbnail_bar(&self) {
        let imp = self.imp();
        if let Some(source) = imp.bar_peek.take() {
            source.remove();
        }
        imp.bar_revealer.set_reveal_child(true);
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(BAR_PEEK_MS), move || {
            let Some(viewer) = weak.upgrade() else { return };
            viewer.imp().bar_peek.take();
            viewer.reveal_near(viewer.imp().pointer.get());
        });
        imp.bar_peek.replace(Some(source));
    }

    /// Calls `callback` with the side, as a setting value, that the thumbnail bar was dragged to.
    pub fn connect_thumbnail_side_requested<F: Fn(&Self, &str) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("thumbnail-side-requested", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let side = values[1].get::<String>().expect("side argument");
            callback(&viewer, &side);
            None
        })
    }

    /// Calls `callback` with the thumbnail size, in pixels, the thumbnail bar was resized to.
    pub fn connect_thumbnail_size_requested<F: Fn(&Self, i32) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("thumbnail-size-requested", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let size = values[1].get::<i32>().expect("size argument");
            callback(&viewer, size);
            None
        })
    }

    /// Calls `callback` with the direction of a step past the end of the folder, when that goes on to the next folder.
    pub fn connect_folder_end_reached<F: Fn(&Self, bool) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("folder-end-reached", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let forward = values[1].get::<bool>().expect("direction argument");
            callback(&viewer, forward);
            None
        })
    }

    /// Sets what stepping past the first or last item does.
    pub fn set_folder_end(&self, end: FolderEnd) {
        let imp = self.imp();
        imp.folder_end.set(end);
        self.update_navigation(imp.index.get(), imp.paths.borrow().len());
    }

    /// Enables the previous and next controls for the steps that can be taken from the item at `index`.
    fn update_navigation(&self, index: usize, len: usize) {
        let end = self.imp().folder_end.get();
        let can_step = |forward| end == FolderEnd::NextFolder || step_index(index, len, forward, end).is_some();
        self.imp().controls.set_navigation(can_step(false), can_step(true));
    }

    /// Replaces the items with `paths`, as the folder they come from has changed. The item shown stays shown, at
    /// its new place; if it is gone, the item at its place takes its place instead.
    pub fn update_paths(&self, paths: Vec<PathBuf>) {
        let imp = self.imp();
        let len = paths.len();
        let current = imp.paths.borrow().get(imp.index.get()).cloned();
        let found = current.as_ref().and_then(|current| paths.iter().position(|path| path == current));
        imp.thumbnail_bar.set_paths(&paths);
        imp.paths.replace(paths);
        match found {
            Some(index) => {
                imp.index.set(index);
                imp.thumbnail_bar.select(index);
                imp.position.set_label(&format!("{} / {}", index + 1, len));
                self.update_navigation(index, len);
                self.refresh_status();
            }
            None if len > 0 => self.load(imp.index.get().min(len - 1)),
            None => {}
        }
    }

    /// Stops any video and drops the items, so the viewer holds nothing once it is hidden.
    pub fn close(&self) {
        self.release();
        self.imp().shown_any.set(false);
        self.imp().paths.take();
    }

    /// Calls `callback` with the path of a file dropped on the viewer.
    pub fn connect_file_dropped<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("file-dropped", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&viewer, Path::new(&path));
            None
        })
    }

    /// Calls `callback` when the viewer asks to go back, by its Back button or Escape.
    pub fn connect_back_requested<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("back-requested", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            callback(&viewer);
            None
        })
    }

    /// Sets how long, in milliseconds, a video waits to start after stepping to it. Choosing a video starts it at once.
    pub fn set_video_start_delay(&self, millis: i32) {
        self.imp().video_start_delay.set(millis.max(0) as u64);
    }

    /// Has the next item shown count as stepped to, so a video there waits to start.
    pub fn delay_next_video(&self) {
        self.imp().delay_next.set(true);
    }

    /// Shows the item at `index` of the items it has, if there is one.
    pub fn go_to(&self, index: usize) {
        self.load(index);
    }

    /// Shows the first item, or the last if `last` is set.
    pub fn jump(&self, last: bool) {
        let imp = self.imp();
        let len = imp.paths.borrow().len();
        if len > 0 {
            self.load(if last { len - 1 } else { 0 });
        }
    }

    /// Repeats the video shown, or stops repeating it. The setting applies to later videos too.
    pub fn toggle_repeat(&self) {
        let imp = self.imp();
        let repeat = !imp.repeat.get();
        imp.repeat.set(repeat);
        if let Some(media) = imp.media.borrow().as_ref() {
            media.set_loop(repeat);
        }
    }

    /// Plays the next item when a video ends, or stops doing so.
    pub fn toggle_auto_advance(&self) {
        let imp = self.imp();
        imp.auto_advance.set(!imp.auto_advance.get());
        imp.controls.set_auto_advance(imp.auto_advance.get());
    }

    /// Keeps the controls and the title shown, or lets them hide again.
    pub fn toggle_pinned(&self) {
        let imp = self.imp();
        imp.pin_button.set_active(!imp.pinned.get());
    }

    /// Shows the zoom level always, never, or once the picture is zoomed by hand.
    pub fn set_zoom_indicator(&self, indicator: ZoomIndicator) {
        self.imp().zoom_indicator.set(indicator);
        self.update_zoom_indicator();
    }

    /// Shows the zoom level as the setting asks, for a picture. A video has no zoom level to show.
    fn update_zoom_indicator(&self) {
        let imp = self.imp();
        let shown = match imp.zoom_indicator.get() {
            ZoomIndicator::On => true,
            ZoomIndicator::Off => false,
            ZoomIndicator::Auto => imp.canvas.is_zoomed(),
        };
        imp.zoom_box.set_visible(shown && imp.media.borrow().is_none());
    }

    /// Hides the pointer over the viewer once it has been still for a while, when `on`. Otherwise it is always shown.
    pub fn set_auto_hide_cursor(&self, on: bool) {
        self.imp().auto_hide_cursor.set(on);
        self.wake_cursor();
    }

    /// Shows the pointer, and hides it again after it has been still, when the cursor is set to hide.
    fn wake_cursor(&self) {
        let imp = self.imp();
        if let Some(source) = imp.cursor_hide.take() {
            source.remove();
        }
        self.set_cursor(None);
        if !imp.auto_hide_cursor.get() {
            return;
        }
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(std::time::Duration::from_millis(CURSOR_HIDE_MS), move || {
            let Some(viewer) = weak.upgrade() else { return };
            viewer.imp().cursor_hide.take();
            viewer.set_cursor(gdk::Cursor::from_name("none", None).as_ref());
        });
        imp.cursor_hide.replace(Some(source));
    }

    /// Notes whether the window is fullscreen, which decides where the header goes.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.imp().fullscreen.set(fullscreen);
        self.place_header();
        self.place_status_bar();
        self.refresh_overlay();
    }

    /// Floats the header over the picture, sliding down only while the pointer is near the top, when the
    /// window is fullscreen and the header is not pinned; otherwise it sits above the picture, always shown.
    fn place_header(&self) {
        let imp = self.imp();
        let header = &*imp.header;
        let floating = imp.fullscreen.get() && !imp.pinned.get();
        let overlay = imp.overlay.upcast_ref::<gtk::Widget>();
        let in_overlay = header.parent().as_ref() == Some(overlay);
        if let Some(source) = imp.header_hide.take() {
            source.remove();
        }
        if floating && !in_overlay {
            if header.parent().is_some() {
                imp.header_dock.remove(header);
            }
            // It starts hidden and slides in from the top once the pointer reaches it.
            header.set_reveal_child(false);
            imp.overlay.add_overlay(header);
        } else if !floating && (in_overlay || header.parent().is_none()) {
            if in_overlay {
                imp.overlay.remove_overlay(header);
            }
            imp.header_dock.append(header);
        }
        if floating {
            imp.header_bar.add_css_class("floating");
        } else {
            imp.header_bar.remove_css_class("floating");
            header.set_reveal_child(true);
        }
    }

    /// Shows a band along `side` of the picture, where the thumbnail bar would go if it were dropped now, or takes it away
    /// for `None`.
    fn show_drop_indicator(&self, side: Option<gtk::PositionType>) {
        let Some(indicator) = self.imp().drop_indicator.get() else { return };
        let Some(side) = side else {
            indicator.set_visible(false);
            return;
        };
        let horizontal = matches!(side, gtk::PositionType::Top | gtk::PositionType::Bottom);
        for class in ["side-top", "side-bottom", "side-left", "side-right"] {
            indicator.remove_css_class(class);
        }
        indicator.add_css_class(&format!("side-{}", side_name(side)));
        let (halign, valign) = match side {
            gtk::PositionType::Top => (gtk::Align::Fill, gtk::Align::Start),
            gtk::PositionType::Left => (gtk::Align::Start, gtk::Align::Fill),
            gtk::PositionType::Right => (gtk::Align::End, gtk::Align::Fill),
            _ => (gtk::Align::Fill, gtk::Align::End),
        };
        indicator.set_halign(halign);
        indicator.set_valign(valign);
        // As thick as the bar is, so it shows how much room it takes.
        let thickness = self.imp().thumbnail_bar.measure(if horizontal { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, -1).1.max(48);
        indicator.set_size_request(if horizontal { -1 } else { thickness }, if horizontal { thickness } else { -1 });
        indicator.set_visible(true);
    }

    /// Sets whether the thumbnail bar slides in and out, and how long that takes, in milliseconds.
    pub fn set_bar_animation(&self, enabled: bool, millis: u32) {
        self.imp().bar_revealer.set_transition_duration(if enabled { millis } else { 0 });
    }

    /// Plays later videos with libmpv, or with GTK's own media backend.
    pub fn set_use_mpv(&self, use_mpv: bool) {
        self.imp().use_mpv.set(use_mpv);
    }

    /// Gives later videos played by mpv the mpv command line options in `options`.
    pub fn set_mpv_options(&self, options: &str) {
        self.imp().mpv_options.replace(options.to_owned());
    }

    /// Sets how videos played by mpv are framed: cropped to their picture, and over a blurred backdrop. The video shown is
    /// framed that way now.
    pub fn set_framing(&self, framing: crate::mpv::Framing) {
        let imp = self.imp();
        if imp.framing.replace(framing) == framing {
            return;
        }
        if let Some(stream) = imp.media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>()) {
            stream.set_framing(framing);
        }
    }

    /// Sets which of a file's settings are kept for it.
    pub fn set_remember(&self, remember: Remember) {
        self.imp().remember.set(remember);
    }

    /// Changes the volume of the video shown by `delta`.
    pub fn adjust_volume(&self, delta: f64) {
        self.imp().controls.adjust_volume(delta);
        // The volume display comes up whether or not the volume moved, as it does at either end of the scale.
        if self.imp().media.borrow().is_some() {
            self.note_change(Trigger::Volume);
        }
    }

    /// Changes the turn and mirror of the picture or video shown with `change`. A picture is redrawn from its pixels
    /// and a video gets a filter.
    pub fn reorient(&self, change: impl FnOnce(Orientation) -> Orientation) {
        let imp = self.imp();
        let stream = imp.media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        match stream {
            Some(stream) => stream.set_orientation(change(stream.orientation())),
            None if imp.media.borrow().is_none() => imp.canvas.set_orientation(change(imp.canvas.orientation())),
            None => {}
        }
    }

    /// Zooms the picture shown in, or out.
    pub fn zoom_in(&self) {
        self.imp().canvas.zoom_in();
    }

    /// Zooms in or out a step on the point under the pointer.
    pub fn zoom_in_at_pointer(&self) {
        self.imp().canvas.zoom_in_at_pointer();
    }

    pub fn zoom_out_at_pointer(&self) {
        self.imp().canvas.zoom_out_at_pointer();
    }

    pub fn zoom_out(&self) {
        self.imp().canvas.zoom_out();
    }

    /// Fits the picture shown in a fit mode.
    pub fn set_fit(&self, fit: Fit) {
        self.imp().canvas.set_fit(fit);
    }

    /// Moves to the fit mode after the current one.
    pub fn cycle_fit(&self) {
        self.imp().canvas.cycle_fit();
    }

    /// Makes each zoom step change the scale by `percent`.
    pub fn set_zoom_step(&self, percent: i32) {
        self.imp().canvas.set_zoom_step(percent);
    }

    /// Zooms the picture by `notches` of the scroll wheel, keeping the point under the pointer in place.
    pub fn zoom_wheel(&self, notches: f64) {
        self.imp().canvas.zoom_wheel(notches);
    }

    /// Limits how much fitting to the window enlarges a small picture, to `factor` times its size; 0 has no limit.
    pub fn set_expand_limit(&self, factor: i32) {
        self.imp().canvas.set_expand_limit(f64::from(factor));
    }

    /// Sets where a picture shown at its own size sits: at the top, centred, or with the point under the pointer kept.
    pub fn set_original_focus(&self, focus: OriginalFocus) {
        self.imp().canvas.set_original_focus(focus);
    }

    /// Sets whether zooming out may go below the picture's own size.
    pub fn set_minimum_zoom_unlocked(&self, unlocked: bool) {
        self.imp().canvas.set_minimum_unlocked(unlocked);
    }

    /// Whether a new picture keeps the fit mode last chosen, rather than fitting the window.
    pub fn set_keep_fit(&self, keep: bool) {
        self.imp().canvas.set_keep_fit(keep);
    }

    /// Sets the fit mode a new picture gets, when it is not kept from the last one.
    pub fn set_default_fit(&self, fit: Fit) {
        self.imp().canvas.set_default_fit(fit);
    }

    /// Whether zooming and panning ease to their new place over a few frames, rather than jumping there.
    pub fn set_smooth_scrolling(&self, on: bool) {
        self.imp().canvas.set_smooth_scrolling(on);
    }

    /// Sets how pictures are resampled, nearest or linear, and whether enlarged ones are smoothed too.
    pub fn set_scaling(&self, scaling: Scaling, smooth_upscaling: bool) {
        self.imp().canvas.set_scaling(scaling, smooth_upscaling);
    }

    /// Shows a blurred, dimmed copy of the picture behind it, or not.
    pub fn set_blurred_background(&self, on: bool) {
        self.imp().canvas.set_underlay(on);
    }

    /// Pans the picture shown by `dx` and `dy` pixels.
    pub fn pan(&self, dx: f64, dy: f64) {
        self.imp().canvas.pan(dx, dy);
    }

    /// Pauses the video shown, if there is one.
    pub fn pause(&self) {
        self.imp().controls.pause();
    }

    /// Plays or pauses the video shown, if there is one.
    pub fn toggle_playback(&self) {
        self.imp().controls.toggle_playing();
    }

    /// Mutes or unmutes the video shown, if there is one.
    pub fn toggle_mute(&self) {
        self.imp().controls.toggle_mute();
    }

    /// Moves the video's position by `micros` microseconds.
    /// Changes the playback speed by `change`, from the video's own speed, kept within the speeds allowed.
    pub fn adjust_speed(&self, change: f64) {
        let imp = self.imp();
        let current = imp
            .media
            .borrow()
            .as_ref()
            .and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().and_then(crate::mpv::MpvStream::speed))
            .unwrap_or(1.0);
        self.set_speed(current + change);
    }

    /// Plays the video shown at `speed` times normal speed. Only the mpv player can change speed. The speed is kept
    /// for the video when it is left.
    pub fn set_speed(&self, speed: f64) {
        use crate::keybindings::actions::{MAX_SPEED, MIN_SPEED};
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        let imp = self.imp();
        let stream = imp.media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        if let Some(stream) = stream {
            stream.set_speed(speed);
            // SVP's target rate follows the speed, so its filter is rebuilt for it.
            self.apply_svp(&stream, speed, true);
        }
    }

    /// Sets the SVP settings, and applies them to the video shown.
    pub fn set_svp(&self, config: crate::svp::Config) {
        // Every viewer setting's change arrives here, and only these need the video disturbed.
        if self.imp().svp.borrow().as_ref() == Some(&config) {
            return;
        }
        self.imp().svp.replace(Some(config));
        let stream = self.imp().media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        if let Some(stream) = stream {
            let speed = stream.speed().unwrap_or(1.0);
            self.apply_svp(&stream, speed, false);
        }
        // With no video shown, nothing above has let go of the server.
        if !self.imp().svp.borrow().as_ref().is_some_and(|config| config.enabled) {
            self.imp().svp_manager.take();
        }
    }

    /// Applies SVP to the video `stream` plays at `speed`, once mpv knows the video's frame rate. The filter left by the
    /// video before goes first, as this video's first frame would otherwise wait for it to load, only for it to be replaced.
    fn start_svp(&self, stream: &crate::mpv::MpvStream) {
        if let Err(err) = stream.set_interpolation_filter(crate::svp::FILTER_LABEL, None) {
            eprintln!("rsimgv: SVP could not be removed: {err}");
        }
        self.watch_seeks(stream);
        let (weak_viewer, weak_stream) = (self.downgrade(), stream.downgrade());
        let mut tries = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (Some(viewer), Some(stream)) = (weak_viewer.upgrade(), weak_stream.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            tries += 1;
            if stream.fps().is_some() {
                let speed = stream.speed().unwrap_or(1.0);
                // Stepping through videos need not wait on SVP for ones that are left at once.
                viewer.hold_svp(&stream);
                viewer.apply_svp(&stream, speed, false);
                return glib::ControlFlow::Break;
            }
            if tries >= 100 {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// Keeps SVP out of `stream`'s video while it is skipped through: a seek takes the filter out, and it comes back once
    /// seeks have stopped for the settings' time, or when a video paused in the meantime is played.
    fn watch_seeks(&self, stream: &crate::mpv::MpvStream) {
        let weak = self.downgrade();
        stream.connect_seek_requested(move |stream| {
            let Some(viewer) = weak.upgrade() else { return };
            if viewer.imp().svp_filter.borrow().is_some() {
                viewer.hold_svp(stream);
            }
        });
        // A slow seek takes up some of the time, so it is counted again from when the seek lands.
        let weak = self.downgrade();
        stream.connect_seeking_notify(move |stream| {
            let Some(viewer) = weak.upgrade() else { return };
            if !stream.is_seeking() && viewer.imp().svp_held.get() {
                viewer.hold_svp(stream);
            }
        });
        let weak = self.downgrade();
        stream.connect_playing_notify(move |stream| {
            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();
            if stream.is_playing() && imp.svp_held.get() && imp.svp_timer.borrow().is_none() {
                viewer.end_svp_wait(stream);
            }
        });
    }

    /// Takes the video's SVP filter out for the settings' time after a seek, if they ask for that.
    fn hold_svp(&self, stream: &crate::mpv::MpvStream) {
        let imp = self.imp();
        let delay = imp.svp.borrow().as_ref().filter(|config| config.enabled).and_then(|config| config.seek_bypass);
        let Some(delay) = delay else { return };
        imp.svp_held.set(true);
        if let Err(err) = stream.set_interpolation_filter(crate::svp::FILTER_LABEL, None) {
            eprintln!("rsimgv: SVP could not be removed: {err}");
        }
        self.arm_svp_timer(stream, delay);
    }

    /// Puts the video's SVP filter in place, now the wait for it is over. A video held back and paused waits to be played,
    /// as each seek made while paused would have to wait for the filter otherwise, and it has no motion to smooth.
    fn end_svp_wait(&self, stream: &crate::mpv::MpvStream) {
        let imp = self.imp();
        if imp.svp_held.get() && !stream.is_playing() {
            return;
        }
        imp.svp_held.set(false);
        self.install_svp(stream);
    }

    /// Asks for `text` to be said over the window as a notification, as an error if `error`.
    fn announce(&self, text: &str, error: bool) {
        self.emit_by_name::<()>("notification-requested", &[&text, &error]);
    }

    /// Calls `callback` with the text of each notification the viewer asks for, and whether it is an error.
    pub fn connect_notification_requested<F: Fn(&Self, &str, bool) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("notification-requested", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let text = values[1].get::<String>().unwrap_or_default();
            callback(&viewer, &text, values[2].get().unwrap_or(false));
            None
        })
    }

    /// Writes the SVP script for `stream`'s video at `speed` and has mpv load it, or removes the filter when SVP is off.
    /// Only the SVP settings' plugins and the video's own rate are read. With `speed_changed`, a notification reports the speed
    /// even if SVP stays as it is, and the filter waits for the speed to settle.
    fn apply_svp(&self, stream: &crate::mpv::MpvStream, speed: f64, speed_changed: bool) {
        let imp = self.imp();
        let Some(config) = imp.svp.borrow().clone() else { return };
        // The window says when SVP is turned on or off; here a notification says what it does to the video.
        if !config.enabled {
            self.cancel_svp_timer();
            imp.svp_held.set(false);
            imp.svp_announcement.take();
            imp.svp_manager.take();
            if let Err(err) = stream.set_interpolation_filter(crate::svp::FILTER_LABEL, None) {
                eprintln!("rsimgv: SVP could not be removed: {err}");
            }
            imp.svp_filter.take();
            return;
        }
        let plugins = crate::svp::find_plugins(&config.plugin_directory);
        let usable = if config.nvof { plugins.svp2.is_some() } else { plugins.svp1.is_some() && plugins.svp2.is_some() };
        let Some(source) = stream.fps() else { return };
        if !usable {
            eprintln!("rsimgv: SVP plugins not found in {}", config.plugin_directory.display());
            self.announce(&format!("SVP plugins not found in {}", config.plugin_directory.display()), true);
            return;
        }
        let target = crate::svp::target_fps(&config.rate, source, speed, stream.display_fps());
        let script = crate::svp::script(&plugins, source, target, config.nvof);
        let directory = crate::svp::script_directory();
        let path = directory.join(crate::svp::script_name(&script));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(script.as_str()) {
            let written = crate::svp::create_private_directory(&directory)
                .and_then(|()| std::fs::write(&path, &script));
            if let Err(err) = written {
                eprintln!("rsimgv: could not write the SVP script: {err}");
                self.announce("SVP script could not be written", true);
                return;
            }
            println!("rsimgv: SVP script written to {}", path.display());
        }
        // The server comes first, as SVP's libraries connect to it as soon as the filter loads them. SVP still plays
        // without it, as it did before there was one.
        if imp.svp_manager.borrow().is_none() {
            match crate::svp_manager::shared() {
                Ok(manager) => {
                    imp.svp_manager.replace(Some(manager));
                }
                Err(err) => eprintln!("rsimgv: SVP manager: {err}"),
            }
        }
        let text = format!(
            "SVP: {} -> {} ({:.0}% speed)",
            crate::fs::metadata::format_fps(source),
            crate::fs::metadata::format_fps(target),
            speed * 100.0
        );
        let filter = crate::svp::filter(&path);
        if imp.svp_filter.borrow().as_deref() == Some(filter.as_str()) {
            if speed_changed {
                self.announce(&text, false);
            }
            return;
        }
        imp.svp_filter.replace(Some(filter));
        imp.svp_announcement.replace(Some(text));
        // A filter held back goes in when the hold ends.
        if imp.svp_held.get() {
            return;
        }
        if speed_changed {
            self.arm_svp_timer(stream, SVP_SPEED_SETTLE);
        } else {
            self.install_svp(stream);
        }
    }

    /// Puts the video's SVP filter in place, and says so in a notification if it is new.
    fn install_svp(&self, stream: &crate::mpv::MpvStream) {
        let imp = self.imp();
        let filter = imp.svp_filter.borrow().clone();
        match stream.set_interpolation_filter(crate::svp::FILTER_LABEL, filter.as_deref()) {
            Ok(()) => {
                if let Some(text) = imp.svp_announcement.take() {
                    println!("rsimgv: SVP filter set: {}", filter.as_deref().unwrap_or_default());
                    self.announce(&text, false);
                }
            }
            Err(err) => {
                eprintln!("rsimgv: SVP could not be applied: {err}");
                self.announce("SVP could not be applied", true);
            }
        }
    }

    /// Puts the video's SVP filter in place after `delay`, instead of when any earlier call would have.
    fn arm_svp_timer(&self, stream: &crate::mpv::MpvStream, delay: std::time::Duration) {
        self.cancel_svp_timer();
        let (weak_viewer, weak_stream) = (self.downgrade(), stream.downgrade());
        let timer = glib::timeout_add_local_once(delay, move || {
            let (Some(viewer), Some(stream)) = (weak_viewer.upgrade(), weak_stream.upgrade()) else { return };
            // The timer has fired, so there is nothing left to cancel.
            viewer.imp().svp_timer.take();
            viewer.end_svp_wait(&stream);
        });
        self.imp().svp_timer.replace(Some(timer));
    }

    fn cancel_svp_timer(&self) {
        if let Some(timer) = self.imp().svp_timer.take() {
            timer.remove();
        }
    }

    /// Skips the video by `micros`, and shows how far it has got.
    pub fn seek(&self, micros: i64) {
        // While a skip is on its way the video has not moved yet, so the next one goes on from where that one lands.
        let seeking = self.imp().media.borrow().as_ref().is_some_and(|media| media.is_seeking());
        let from = self.imp().seek_landing.get().filter(|_| seeking).map(|(target, _)| target);
        if let Some(target) = self.imp().controls.seek_by(from, micros) {
            let now = std::time::Instant::now();
            self.imp().seek_landing.set(Some((target, now)));
            self.note_change(Trigger::Seek);
            self.imp().seek_forward.set(micros >= 0);
            self.refresh_overlay();
        }
    }

    /// Sets the layout of the overlay over the item.
    pub fn set_overlay_layout(&self, layout: Layout) {
        let overlay = self.media_overlay();
        if overlay.has_layout(&layout) {
            return;
        }
        overlay.set_layout(&layout);
        // What an element shows of the file is read only while an element is on.
        self.refresh_status();
        self.refresh_overlay();
        self.sync_overlay_tick();
    }

    /// Whether the overlay has anything to keep up to date: an element that is on, or one being edited.
    fn overlay_active(&self) -> bool {
        self.media_overlay().any_enabled() || super::media_overlay::preview().is_some()
    }

    /// Whether the timer that keeps the overlay up to date should run: there is something to keep up to date, on an item that is
    /// shown, in a viewer that is on screen.
    fn overlay_ticking(&self) -> bool {
        self.overlay_active() && self.is_mapped() && self.shown_path().is_some()
    }

    /// Starts the timer that keeps the overlay up to date while there is something to, and stops it when there is not.
    fn sync_overlay_tick(&self) {
        let imp = self.imp();
        if !self.overlay_ticking() {
            if let Some(tick) = imp.overlay_tick.take() {
                tick.remove();
            }
        } else if imp.overlay_tick.borrow().is_none() {
            let weak = self.downgrade();
            let tick = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                let Some(viewer) = weak.upgrade() else { return glib::ControlFlow::Break };
                if !viewer.overlay_ticking() {
                    // Ended on its own, so there is nothing to cancel.
                    viewer.imp().overlay_tick.take();
                    return glib::ControlFlow::Break;
                }
                viewer.refresh_overlay();
                glib::ControlFlow::Continue
            });
            imp.overlay_tick.replace(Some(tick));
        }
    }

    fn media_overlay(&self) -> &MediaOverlay {
        let imp = self.imp();
        imp.media_overlay.get_or_init(|| MediaOverlay::new(&imp.overlay))
    }

    /// The value of each property a template can read, for the item shown. A property the item has no value for is left
    /// out. For a moment after a skip, the position is where the skip went, as the video takes a little while to get
    /// there.
    fn overlay_values(&self) -> std::collections::HashMap<&'static str, String> {
        let imp = self.imp();
        let mut values = std::collections::HashMap::new();
        let Some(path) = self.shown_path() else { return values };
        values.insert("path", path.to_string_lossy().into_owned());
        values.insert("playlist-pos", (imp.index.get() + 1).to_string());
        values.insert("playlist-count", imp.paths.borrow().len().to_string());
        for (name, label) in [
            ("filename", &imp.status_name),
            ("resolution", &imp.status_resolution),
            ("fps", &imp.status_fps),
            ("file-size", &imp.status_size),
        ] {
            values.insert(name, label.label().to_string());
        }
        // How the item is turned, and for a picture how far it is zoomed.
        let media = imp.media.borrow().clone();
        let orientation = match media.as_ref().map(|media| media.downcast_ref::<crate::mpv::MpvStream>().map(crate::mpv::MpvStream::orientation)) {
            Some(video) => video,
            None if imp.canvas.texture().is_some() => Some(imp.canvas.orientation()),
            None => None,
        };
        if let Some(orientation) = orientation {
            let degrees = orientation.degrees();
            values.insert("rotation", degrees.to_string());
            values.insert("rotation-percent", (u32::from(degrees) * 100 / 360).to_string());
            values.insert("rotation-name", crate::overlay::rotation_name(degrees).to_owned());
            if orientation.is_mirrored() {
                values.insert("mirrored", "yes".to_owned());
            }
        }
        if media.is_none() && imp.canvas.texture().is_some() {
            values.insert("zoom", format!("{:.0}", imp.zoom_scale.get() * 100.0));
        }
        let Some(media) = media else { return values };
        let landing = imp.seek_landing.get().filter(|(_, when)| when.elapsed() < SEEK_SETTLE);
        let position = landing.map_or_else(|| media.timestamp(), |(target, _)| target);
        let duration = media.duration();
        values.insert("state", if media.is_ended() { "Ended" } else if media.is_playing() { "Playing" } else { "Paused" }.to_owned());
        // Skipping shows the way it went, until the video has caught up with it.
        let icon = match (landing.is_some(), imp.seek_forward.get()) {
            (true, true) => "⏭️",
            (true, false) => "⏮️",
            _ if media.is_ended() => "⏮️",
            _ if media.is_playing() => "▶️",
            _ => "⏸️",
        };
        values.insert("state-icon", icon.to_owned());
        // Until the video has loaded, there is nothing of it to tell but that.
        if !media.is_prepared() {
            values.insert("loading", "Loading...".to_owned());
            return values;
        }
        values.insert("time-pos", clock(position));
        if duration > 0 {
            values.insert("duration", clock(duration));
            values.insert("time-remaining", clock(duration - position.clamp(0, duration)));
            values.insert("percent-pos", format!("{:.0}", fraction(position, duration) * 100.0));
        }
        let speed = match media.downcast_ref::<crate::mpv::MpvStream>() {
            Some(stream) => stream.speed(),
            None => Some(1.0),
        };
        if let Some(speed) = speed {
            let speed_text = format!("{speed:.2}").trim_end_matches('0').trim_end_matches('.').to_owned();
            if (speed - 1.0).abs() > 1e-6 {
                values.insert("speed-changed", speed_text.clone());
            }
            values.insert("speed-fraction", crate::overlay::speed_fraction(speed));
            values.insert("speed-percent", format!("{:.0}", speed * 100.0));
            values.insert("speed", speed_text);
        }
        let (volume, muted) = imp.controls.audio();
        values.insert("volume", format!("{:.0}", volume * 100.0));
        if !media.has_audio() {
            values.insert("no-audio", "yes".to_owned());
        } else if muted {
            values.insert("mute", "muted".to_owned());
        }
        values
    }

    /// Brings the overlay's elements up or down for the item shown, and writes their text.
    fn refresh_overlay(&self) {
        let imp = self.imp();
        let overlay = self.media_overlay();
        if !self.overlay_active() {
            return;
        }
        if self.shown_path().is_none() {
            overlay.hide_all();
            return;
        }
        self.track_changes();
        let values = self.overlay_values();
        let media = imp.media.borrow().clone();
        let position_fraction = media.as_ref().map_or(0.0, |media| {
            let landing = imp.seek_landing.get().filter(|(_, when)| when.elapsed() < SEEK_SETTLE);
            fraction(landing.map_or_else(|| media.timestamp(), |(target, _)| target), media.duration())
        });
        let lookup = |name: &str| values.get(name).cloned();
        overlay.update(&Frame {
            values: &lookup,
            fraction: position_fraction,
            is_video: media.is_some(),
            playing: media.as_ref().is_some_and(|media| media.is_playing()),
            fullscreen: imp.fullscreen.get(),
            volume_fraction: if imp.controls.audio().1 { 0.0 } else { imp.controls.audio().0 },
            has_audio: media.as_ref().is_none_or(|media| media.has_audio()),
            since_change: imp.changed_at.get().map(|when| when.map(|when| when.elapsed())),
        });
    }

    /// Notes that `trigger`'s change just happened, for the elements that come up after it.
    fn note_change(&self, trigger: Trigger) {
        let imp = self.imp();
        let mut changed = imp.changed_at.get();
        changed[trigger as usize] = Some(std::time::Instant::now());
        imp.changed_at.set(changed);
    }

    /// Looks for a change in the volume, speed, rotation or zoom since the last look, which brings up the displays of them.
    /// The first look at an item only notes where each stands, so what a file's settings put there is not a change.
    fn track_changes(&self) {
        let imp = self.imp();
        let media = imp.media.borrow().clone();
        let stream = media.as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        let orientation = match (&stream, &media) {
            (Some(stream), _) => Some(stream.orientation()),
            (None, None) if imp.canvas.texture().is_some() => Some(imp.canvas.orientation()),
            _ => None,
        };
        let audio = imp.controls.audio();
        let current = [
            (Trigger::Volume, media.as_ref().map(|_| if audio.1 { -1.0 } else { audio.0 })),
            (Trigger::Speed, stream.as_ref().and_then(crate::mpv::MpvStream::speed)),
            (Trigger::Rotation, orientation.map(|orientation| f64::from(orientation.degrees()) + if orientation.is_mirrored() { 1000.0 } else { 0.0 })),
            (Trigger::Zoom, (media.is_none() && imp.canvas.texture().is_some()).then(|| imp.zoom_scale.get())),
        ];
        let settling = imp.zoom_settles.get().is_some_and(|until| std::time::Instant::now() < until);
        let mut watched = imp.watched.get();
        for (trigger, value) in current {
            let seen = &mut watched[trigger as usize];
            match (*seen, value) {
                (_, None) => *seen = None,
                (None, Some(value)) => *seen = Some(value),
                (Some(before), Some(value)) if (before - value).abs() > 1e-6 => {
                    *seen = Some(value);
                    if !(trigger == Trigger::Zoom && settling) {
                        self.note_change(trigger);
                    }
                }
                _ => {}
            }
        }
        imp.watched.set(watched);
    }

    /// Takes the overlay's elements down, as the item they showed is gone.
    fn hide_overlay(&self) {
        let imp = self.imp();
        imp.seek_landing.take();
        imp.changed_at.set([None; TRIGGERS]);
        imp.watched.set([None; TRIGGERS]);
        if let Some(overlay) = imp.media_overlay.get() {
            overlay.hide_all();
        }
    }

    /// Places the video controls by the layout instead of at the bottom, and lets them be dragged when it is unlocked:
    /// pressing in the middle moves them, and pressing at the left or right edge changes their width.
    fn setup_controls_placement(&self) {
        let imp = self.imp();
        let weak = self.downgrade();
        imp.overlay.connect_get_child_position({
            let weak = weak.clone();
            move |_, widget| {
                let viewer = weak.upgrade()?;
                if widget != viewer.imp().controls_revealer.upcast_ref::<gtk::Widget>() {
                    return None;
                }
                let (x, y, width, height) = viewer.controls_rect()?;
                Some(gdk::Rectangle::new(x as i32, y as i32, width.ceil() as i32, height.ceil() as i32))
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        drag.connect_drag_begin({
            let weak = weak.clone();
            move |gesture, x, _| {
                let Some(viewer) = weak.upgrade() else { return };
                let imp = viewer.imp();
                let layout = imp.controls_layout.get();
                if layout.locked {
                    return;
                }
                let width = f64::from(imp.controls_revealer.width());
                let mode = if x < CONTROLS_GRAB {
                    ControlsDrag::ResizeLeft
                } else if x > width - CONTROLS_GRAB {
                    ControlsDrag::ResizeRight
                } else {
                    ControlsDrag::Move
                };
                imp.controls_drag.set(Some((mode, layout, width)));
                // The controls are only moved while they are unlocked, so their buttons do not get the press.
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        drag.connect_drag_update({
            let weak = weak.clone();
            move |_, dx, dy| {
                let Some(viewer) = weak.upgrade() else { return };
                let imp = viewer.imp();
                let Some((mode, start, start_width)) = imp.controls_drag.get() else { return };
                let (frame_width, frame_height) = (f64::from(imp.overlay.width()).max(1.0), f64::from(imp.overlay.height()).max(1.0));
                let mut layout = start;
                match mode {
                    ControlsDrag::Move => {
                        layout.x = (start.x + dx / frame_width).clamp(0.0, 1.0);
                        layout.y = (start.y + dy / frame_height).clamp(0.0, 1.0);
                    }
                    ControlsDrag::ResizeLeft | ControlsDrag::ResizeRight => {
                        // The controls grow around their centre, so the edge moves half as far as the width changes.
                        let grow = if matches!(mode, ControlsDrag::ResizeRight) { 2.0 * dx } else { -2.0 * dx };
                        layout.width = ((start_width + grow) / frame_width).clamp(CONTROLS_MIN_WIDTH, 1.0);
                    }
                }
                imp.controls_layout.set(layout);
                imp.overlay.queue_resize();
            }
        });
        drag.connect_drag_end({
            let weak = weak.clone();
            move |_, _, _| {
                let Some(viewer) = weak.upgrade() else { return };
                if viewer.imp().controls_drag.take().is_some() {
                    let layout = viewer.imp().controls_layout.get();
                    viewer.emit_by_name::<()>("controls-layout-changed", &[&layout.x, &layout.y, &layout.width]);
                }
            }
        });
        imp.controls_revealer.add_controller(drag);

        // While unlocked, the pointer shows what a press would do.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let weak = weak.clone();
            move |_, x, _| {
                let Some(viewer) = weak.upgrade() else { return };
                let imp = viewer.imp();
                let cursor = if imp.controls_layout.get().locked {
                    None
                } else if x < CONTROLS_GRAB || x > f64::from(imp.controls_revealer.width()) - CONTROLS_GRAB {
                    Some("ew-resize")
                } else {
                    Some("move")
                };
                imp.controls_revealer.set_cursor_from_name(cursor);
            }
        });
        imp.controls_revealer.add_controller(motion);
    }

    /// Sets where the video controls sit and whether they can be dragged.
    pub fn set_controls_layout(&self, layout: ControlsLayout) {
        let imp = self.imp();
        if imp.controls_layout.get() == layout {
            return;
        }
        imp.controls_layout.set(layout);
        if layout.locked {
            imp.controls.remove_css_class("controls-unlocked");
        } else {
            imp.controls.add_css_class("controls-unlocked");
        }
        imp.controls_revealer.set_cursor_from_name(None);
        imp.overlay.queue_resize();
        self.reveal_near(imp.pointer.get());
    }

    /// Calls `callback` with the centre and width of the video controls, as shares of the frame, when dragging them ends.
    pub fn connect_controls_layout_changed<F: Fn(&Self, f64, f64, f64) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("controls-layout-changed", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            callback(&viewer, values[1].get().unwrap_or(0.5), values[2].get().unwrap_or(0.92), values[3].get().unwrap_or(0.0));
            None
        })
    }

    /// The left, top, width and height of the video controls in the frame that holds them, or `None` while it has no
    /// size yet.
    fn controls_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let imp = self.imp();
        let frame = (f64::from(imp.overlay.width()), f64::from(imp.overlay.height()));
        if frame.0 <= 0.0 || frame.1 <= 0.0 {
            return None;
        }
        let layout = imp.controls_layout.get();
        let (_, natural, _, _) = imp.controls.measure(gtk::Orientation::Horizontal, -1);
        let width = controls_width(frame.0, f64::from(natural), &layout);
        let (_, height, _, _) = imp.controls.measure(gtk::Orientation::Vertical, width.ceil() as i32);
        let height = f64::from(height);
        let (x, y) = controls_origin(frame, (width, height), &layout);
        Some((x, y, width, height))
    }

    /// Whether the pointer at `(x, y)` in the viewer is near enough to the video controls to bring them up.
    fn near_controls(&self, x: f64, y: f64) -> bool {
        let Some((left, top, width, height)) = self.controls_rect() else { return false };
        let Some(point) = self.compute_point(&*self.imp().overlay, &gtk::graphene::Point::new(x as f32, y as f32)) else { return false };
        let (x, y) = (f64::from(point.x()), f64::from(point.y()));
        x > left - CONTROLS_REVEAL_MARGIN && x < left + width + CONTROLS_REVEAL_MARGIN && y > top - CONTROLS_REVEAL_MARGIN && y < top + height + CONTROLS_REVEAL_MARGIN
    }

    /// Calls `callback` with the path of each item the viewer shows, when it changes to it.
    pub fn connect_item_changed<F: Fn(&Self, &Path) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("item-changed", false, move |values| {
            let viewer = values[0].get::<MediaViewer>().expect("viewer argument");
            let path = values[1].get::<String>().expect("path argument");
            callback(&viewer, Path::new(&path));
            None
        })
    }

    /// The item shown, at its own size: the picture, or the video's current frame, captured by mpv when it plays
    /// the video. `None` when nothing is shown, or a video has no frame yet.
    pub fn screenshot(&self) -> Option<gdk::Texture> {
        let imp = self.imp();
        let Some(media) = imp.media.borrow().clone() else { return imp.canvas.texture() };
        // mpv captures the frame itself, at the video's own size.
        if let Some(stream) = media.downcast_ref::<crate::mpv::MpvStream>() {
            return stream.screenshot();
        }
        let frame = media.current_image();
        if let Ok(texture) = frame.clone().downcast::<gdk::Texture>() {
            return Some(texture);
        }
        // Any other frame is drawn at its own size and rendered to a texture by the window's renderer.
        let (width, height) = (frame.intrinsic_width(), frame.intrinsic_height());
        if width <= 0 || height <= 0 {
            return None;
        }
        let snapshot = gtk::Snapshot::new();
        frame.snapshot(&snapshot, f64::from(width), f64::from(height));
        let node = snapshot.to_node()?;
        let renderer = self.native()?.renderer()?;
        Some(renderer.render_texture(node, Some(&gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32))))
    }

    /// Whether the item shown is a video.
    pub fn shows_video(&self) -> bool {
        self.imp().media.borrow().is_some()
    }

    /// The item shown: its path, its size, and for a video, its playback. `None` when nothing is shown.
    pub fn shown_item(&self) -> Option<ShownItem> {
        let imp = self.imp();
        let path = imp.paths.borrow().get(imp.index.get()).cloned()?;
        let media = imp.media.borrow().clone();
        let Some(media) = media else {
            // A picture is drawn at its own size, so its texture is that size.
            let texture = imp.canvas.texture()?;
            return Some(ShownItem { path, width: texture.width(), height: texture.height(), playback: None });
        };
        let duration = media.duration();
        let mpv = media.downcast_ref::<crate::mpv::MpvStream>();
        let playback = Playback {
            position: media.timestamp() as f64 / MICROS,
            duration: (duration > 0).then(|| duration as f64 / MICROS),
            fps: mpv.and_then(|stream| stream.fps()),
            // GTK's own backend always plays at normal speed.
            speed: match mpv {
                Some(stream) => stream.speed(),
                None => Some(1.0),
            },
        };
        Some(ShownItem { path, width: media.intrinsic_width(), height: media.intrinsic_height(), playback: Some(playback) })
    }

    fn request_back(&self) {
        self.emit_by_name::<()>("back-requested", &[]);
    }

    /// Shows or hides the status bar under the item: its position, name, resolution, frame rate and file size.
    pub fn set_status_bar(&self, shown: bool) {
        let imp = self.imp();
        imp.status_shown.set(shown);
        imp.status_bar.set_visible(shown);
        self.refresh_status();
    }

    /// The parts of the viewer that float over the item, which KDE's blur is drawn behind: the
    /// header in fullscreen (unless pinned), the video controls, the thumbnail bar when it floats, and the overlay's elements
    /// that ask for it.
    pub fn floating_widgets(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        let mut widgets = Vec::new();
        if imp.fullscreen.get() {
            if !imp.pinned.get() && imp.header.reveals_child() {
                widgets.push(imp.header.clone().upcast());
            }
        }
        if imp.controls_revealer.reveals_child() {
            widgets.push(imp.controls_revealer.clone().upcast());
        }
        if imp.bar_shown.get() && imp.bar_overlay.get() {
            widgets.push(imp.thumbnail_bar.clone().upcast());
        }
        if let Some(overlay) = imp.media_overlay.get() {
            widgets.extend(overlay.blurred());
        }
        widgets
    }

    /// Puts the status bar under the item. In fullscreen it is left out, as the overlay's status element is there
    /// in its place.
    fn place_status_bar(&self) {
        let imp = self.imp();
        let bar: &gtk::Box = &imp.status_bar;
        if bar.parent().is_some() {
            bar.unparent();
        }
        if imp.fullscreen.get() {
            return;
        }
        // Beside a thumbnail bar at the left or right, the status bar shrinks to the picture, as it does beside
        // the playlist.
        let beside = imp.bar_shown.get()
            && !imp.bar_overlay.get()
            && matches!(imp.bar_side.get(), Some(gtk::PositionType::Left | gtk::PositionType::Right));
        if beside {
            imp.picture_column.append(bar);
        } else {
            imp.content_column.append(bar);
        }
    }

    /// The path of the item shown.
    fn shown_path(&self) -> Option<PathBuf> {
        let imp = self.imp();
        imp.paths.borrow().get(imp.index.get()).cloned()
    }

    /// Fills the status bar for the item shown. Its resolution and frame rate come from its metadata, which is read in
    /// the background the first time; the bar is filled again once it is.
    fn refresh_status(&self) {
        let imp = self.imp();
        let overlay_wants_it = imp.media_overlay.get().is_some_and(MediaOverlay::any_enabled);
        if !imp.status_shown.get() && !overlay_wants_it {
            return;
        }
        let Some(path) = self.shown_path() else { return };
        let (index, len) = (imp.index.get(), imp.paths.borrow().len());
        imp.status_counter.set_label(&format!("[{}/{}]", index + 1, len));
        imp.status_name.set_label(&path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned()));
        // The file is read on a worker, so a slow or remote file does not hold up the viewer.
        let stat_path = path.clone();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let stat = gio::spawn_blocking(move || std::fs::metadata(&stat_path).map(|file| (file.len(), mtime_secs(&file))))
                .await;
            let Some(viewer) = weak.upgrade() else { return };
            // The file is only shown if it is still the one shown when its stat returns.
            if let Ok(Ok((size, mtime))) = stat {
                viewer.show_file_status(&path, size, mtime);
            }
        });
    }

    /// Shows the size and metadata of the shown file at `path`, which has `size` bytes and was modified at `mtime`.
    fn show_file_status(&self, path: &Path, size: u64, mtime: i64) {
        let imp = self.imp();
        if self.shown_path().as_deref() != Some(path) {
            return;
        }
        imp.status_size.set_label(&glib::format_size(size).to_string());
        let path = path.to_path_buf();

        let metadata = MediaInfo::shared().get(&path, mtime, size).or_else(|| {
            // The request is only wanted while this item is still the one shown.
            let (weak_wanted, wanted_path) = (self.downgrade(), path.clone());
            let weak_ready = self.downgrade();
            MediaInfo::shared().request(
                &path,
                mtime,
                size,
                move || weak_wanted.upgrade().is_some_and(|viewer| viewer.shown_path().as_deref() == Some(wanted_path.as_path())),
                move || {
                    if let Some(viewer) = weak_ready.upgrade() {
                        viewer.refresh_status();
                    }
                },
            )
        });
        let resolution = metadata.as_ref().and_then(|metadata| Some(format!("{} × {}", metadata.width?, metadata.height?)));
        // Only a video has a frame rate worth showing; a picture's would be meaningless.
        let fps = metadata
            .as_ref()
            .filter(|_| is_video(&path))
            .and_then(|metadata| metadata.frame_rate)
            .map(crate::fs::metadata::format_fps);
        for (label, text) in [(&imp.status_resolution, resolution), (&imp.status_fps, fps)] {
            label.set_label(text.as_deref().unwrap_or_default());
            label.set_visible(text.is_some());
        }
    }

    /// Puts the largest thumbnail already made for the video at `path` behind it, blurred, if there is one. None is made.
    fn show_backdrop(&self, path: &Path) {
        let (weak, path) = (self.downgrade(), path.to_path_buf());
        glib::spawn_future_local(async move {
            let stamp = crate::fs::thumbnail::stamp(&path);
            let Some(texture) = Thumbnails::shared().best_existing(&path, stamp).await else { return };
            let Some(viewer) = weak.upgrade() else { return };
            // Only if the video is still the one shown, and does not already cover the view with its own frames.
            let covered = viewer.imp().media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>()).is_some_and(|stream| stream.has_frame());
            if viewer.shown_path().as_deref() == Some(path.as_path()) && viewer.imp().media.borrow().is_some() && !covered {
                viewer.imp().backdrop.set_texture(Some(&texture));
            }
        });
    }

    /// The scale of the surface the picture is on, which may be fractional, or the widget's whole-number scale before it has one.
    fn display_scale(&self) -> f64 {
        let picture = &self.imp().picture;
        picture.native().and_then(|native| native.surface()).map_or_else(|| f64::from(picture.scale_factor()), |surface| surface.scale())
    }

    /// Gives the video shown the display's scale.
    fn apply_display_scale(&self) {
        let stream = self.imp().media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        if let Some(stream) = stream {
            stream.set_scale(self.display_scale());
        }
    }

    /// Follows the scale of the surface the picture is on, so a video is rendered at the right size after the window moves to
    /// a display with another scale.
    fn watch_display_scale(&self) {
        let imp = self.imp();
        let Some(surface) = imp.picture.native().and_then(|native| native.surface()) else { return };
        if imp.scale_watch.borrow().as_ref().is_some_and(|(watched, _)| *watched == surface) {
            return;
        }
        let weak = self.downgrade();
        let handler = surface.connect_scale_notify(move |_| {
            if let Some(viewer) = weak.upgrade() {
                viewer.apply_display_scale();
            }
        });
        if let Some((old, old_handler)) = imp.scale_watch.replace(Some((surface, handler))) {
            old.disconnect(old_handler);
        }
        self.apply_display_scale();
    }

    /// Shows the item at `index`, replacing the one before it.
    fn load(&self, index: usize) {
        let imp = self.imp();
        let Some(path) = imp.paths.borrow().get(index).cloned() else { return };
        let len = imp.paths.borrow().len();
        self.release();
        imp.delay_video.set(imp.delay_next.take() && imp.shown_any.get() && imp.video_start_delay.get() > 0);
        imp.index.set(index);
        imp.thumbnail_bar.select(index);

        let name = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        imp.filename.set_label(&name);
        imp.position.set_label(&format!("{} / {}", index + 1, len));
        self.refresh_status();
        self.update_navigation(index, len);

        // The file's settings are read on a worker thread: the moviehash reads the file, and the settings file is parsed.
        // With none kept, the file is not read.
        let keep = imp.remember.get().enabled;
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let settings = gio::spawn_blocking({
                let path = path.clone();
                move || if keep { FileSettings::for_path(&path).ok() } else { None }
            })
            .await
            .ok()
            .flatten();
            if let Some(viewer) = weak.upgrade() {
                viewer.finish_load(index, path, settings);
            }
        });
    }

    /// Shows the item at `index`, `path`, once its settings are read. Nothing is shown if another item was asked for
    /// in the meantime.
    fn finish_load(&self, index: usize, path: PathBuf, settings: Option<FileSettings>) {
        let imp = self.imp();
        if imp.index.get() != index || imp.paths.borrow().get(index) != Some(&path) {
            return;
        }
        let video = is_video(&path);
        // A video stepped to from another item starts after a short wait, which the next step cancels.
        if video && imp.delay_video.replace(false) {
            let weak = self.downgrade();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(imp.video_start_delay.get()), move || {
                let Some(viewer) = weak.upgrade() else { return };
                viewer.imp().video_start.take();
                viewer.finish_load(index, path, settings);
            });
            imp.video_start.replace(Some(source));
            return;
        }
        imp.shown_any.set(true);
        if video {
            // libmpv plays the video if it is chosen and starts; otherwise GTK's media backend does.
            let media: gtk::MediaStream = imp
                .use_mpv
                .get()
                .then(|| crate::mpv::MpvStream::for_path(&path, &imp.mpv_options.borrow()))
                .flatten()
                .map(|stream| {
                    // Each video plays at the speed it was left at, or normal speed.
                    let speed = settings.as_ref().filter(|_| imp.remember.get().speed).and_then(|settings| settings.get_f64("speed")).unwrap_or(1.0);
                    stream.set_speed(speed);
                    stream.set_framing(imp.framing.get());
                    // Once its frames are up the stream fills the view itself, with the picture over its backdrop or black, so the
                    // thumbnail behind it is not drawn under every frame. GTK's own player leaves bars, which the thumbnail fills.
                    stream.connect_first_frame({
                        let weak = self.downgrade();
                        move |stream| {
                            let Some(viewer) = weak.upgrade() else { return };
                            let shown = viewer.imp().media.borrow().as_ref().is_some_and(|media| media == stream.upcast_ref::<gtk::MediaStream>());
                            if shown {
                                viewer.imp().backdrop.set_texture(None);
                            }
                        }
                    });
                    if let Some(orientation) = settings.as_ref().filter(|_| imp.remember.get().rotation).and_then(saved_orientation) {
                        stream.set_orientation(orientation);
                    }
                    self.start_svp(&stream);
                    // Frames are rendered at device pixels, so the video and its OSD are drawn at the display's scale.
                    stream.set_scale(self.display_scale());
                    stream.upcast()
                })
                .unwrap_or_else(|| gtk::MediaFile::for_filename(&path).upcast());
            // The end of a video either repeats it or, with auto-advance on, moves to the next item. The step
            // waits until the notification has returned, since it releases the video that sent it.
            media.connect_ended_notify({
                let weak = self.downgrade();
                move |media| {
                    let Some(viewer) = weak.upgrade() else { return };
                    if !media.is_ended() || viewer.imp().repeat.get() || !viewer.imp().auto_advance.get() {
                        return;
                    }
                    glib::idle_add_local_once(move || viewer.step(true));
                }
            });
            media.set_loop(imp.repeat.get());
            // Below the thumbnail generator's higher levels, nothing is made while a video plays.
            media.connect_playing_notify(|media| Thumbnails::shared().set_media_playing(media.is_playing()));
            media.connect_error_notify(|media| {
                if let Some(err) = media.error() {
                    eprintln!("rsimgv: {err}");
                }
            });
            if let Some(page) = imp.video_page.get() {
                imp.pages.set_visible_child(page);
            }
            self.show_backdrop(&path);
            // The stream draws the video fitted to the area itself, and renders only the picture, so mpv's OSD sits in the video's corner.
            let fit = if media.is::<crate::mpv::MpvStream>() { gtk::ContentFit::Fill } else { gtk::ContentFit::Contain };
            imp.picture.set_content_fit(fit);
            imp.picture.set_paintable(Some(&media));
            // A video with a volume of its own starts at it. The others start at the level the user had before any video's
            // own, so one video's volume does not carry over to the next.
            let remember = imp.remember.get();
            if remember.enabled && remember.volume {
                let base = imp.audio_base.get().unwrap_or_else(|| imp.controls.audio());
                imp.audio_base.set(Some(base));
                let saved = settings.as_ref().and_then(|settings| settings.get_f64("volume"));
                match saved {
                    Some(volume) => imp.controls.set_audio(volume, settings.as_ref().and_then(|settings| settings.get_bool("muted")).unwrap_or(false)),
                    None => imp.controls.set_audio(base.0, base.1),
                }
            } else {
                imp.audio_base.set(None);
            }
            imp.audio_at_load.set(imp.controls.audio());
            imp.controls.set_media(Some(&media));
            // Playing is requested once the stream is ready, since a request made before then is dropped. It starts
            // where it was left.
            let position = settings.as_ref().filter(|_| imp.remember.get().position).and_then(|settings| settings.get_f64("position"));
            media.connect_prepared_notify(move |media| {
                if !media.is_prepared() {
                    return;
                }
                if let Some(position) = position {
                    let duration = media.duration() as f64 / 1_000_000.0;
                    if media.is_seekable() && (duration <= 0.0 || position < duration - POSITION_MARGIN) {
                        media.seek((position * 1_000_000.0) as i64);
                    }
                }
                media.play();
            });
            imp.media.replace(Some(media));
            self.refresh_overlay();
        } else {
            imp.pages.set_visible_child(&*imp.canvas);
            let remember = imp.remember.get();
            let placement = settings
                .as_ref()
                .filter(|_| remember.zoom)
                .and_then(Placement::from_settings)
                .map(|placement| if remember.pan { placement } else { placement.centered() });
            let orientation = settings.as_ref().filter(|_| remember.rotation).and_then(saved_orientation);
            self.show_picture(index, &path, placement, orientation);
            self.prefetch_neighbours(index);
        }
        imp.file_settings.replace(settings);
        imp.controls_revealer.set_visible(video);
        self.sync_overlay_tick();
        self.update_zoom_indicator();
        self.emit_by_name::<()>("item-changed", &[&path.to_string_lossy().into_owned()]);
    }

    /// Shows the picture at `path`, the item at `index`, once it is decoded. A picture decoded ahead is shown at once. A
    /// decode that finishes after another item is shown is not shown.
    fn show_picture(&self, index: usize, path: &Path, placement: Option<Placement>, orientation: Option<Orientation>) {
        let imp = self.imp();
        let ahead = {
            let mut prefetched = imp.prefetched.borrow_mut();
            prefetched.iter().position(|(ahead, _)| ahead == path).map(|position| prefetched.remove(position).1)
        };
        if let Some(pixels) = ahead {
            self.set_picture(&pixels, placement, orientation);
            return;
        }
        let weak = self.downgrade();
        let path = path.to_path_buf();
        glib::spawn_future_local(async move {
            let Some(pixels) = decode_picture(path).await else { return };
            let Some(viewer) = weak.upgrade() else { return };
            if viewer.imp().index.get() == index && viewer.imp().media.borrow().is_none() {
                viewer.set_picture(&pixels, placement, orientation);
            }
        });
    }

    /// Puts the decoded picture on the canvas, at the placement it was left at.
    fn set_picture(&self, pixels: &Pixels, placement: Option<Placement>, orientation: Option<Orientation>) {
        let imp = self.imp();
        imp.canvas.set_texture(Some(pixels.texture()));
        // The zoom settles as the picture is first laid out, which is no change of the user's.
        imp.zoom_settles.set(Some(std::time::Instant::now() + std::time::Duration::from_millis(500)));
        // Turned first, as the placement is of the picture as it was shown turned.
        if let Some(orientation) = orientation {
            imp.canvas.set_orientation(orientation);
        }
        if let Some(placement) = placement {
            imp.canvas.restore(placement);
        }
        self.update_zoom_indicator();
    }

    /// Decodes the pictures either side of the item at `index`, so that stepping to them shows them at once.
    fn prefetch_neighbours(&self, index: usize) {
        let imp = self.imp();
        if !imp.preloading.get() {
            return;
        }
        let paths = imp.paths.borrow();
        let len = paths.len();
        let end = imp.folder_end.get();
        let ahead: Vec<PathBuf> = [step_index(index, len, false, end), step_index(index, len, true, end)]
            .into_iter()
            .flatten()
            .filter_map(|neighbour_index| paths.get(neighbour_index).cloned())
            .filter(|path| !is_video(path))
            .collect();
        drop(paths);
        for path in ahead {
            let weak = self.downgrade();
            glib::spawn_future_local(async move {
                let Some(pixels) = decode_picture(path.clone()).await else { return };
                let Some(viewer) = weak.upgrade() else { return };
                // Turned off while this was being decoded.
                if !viewer.imp().preloading.get() {
                    return;
                }
                let mut prefetched = viewer.imp().prefetched.borrow_mut();
                if prefetched.len() >= PREFETCHED {
                    prefetched.remove(0);
                }
                prefetched.push((path, pixels));
            });
        }
    }

    /// Clears the shown item: a video is paused and released, and the picture is emptied.
    fn release(&self) {
        let imp = self.imp();
        if let Some(source) = imp.video_start.take() {
            source.remove();
        }
        self.hide_overlay();
        self.cancel_svp_timer();
        imp.svp_held.set(false);
        imp.svp_filter.take();
        imp.svp_announcement.take();
        let settings = imp.file_settings.take();
        let media = imp.media.take();
        let mut changes = match &media {
            Some(media) => {
                let mut changes = video_settings(media);
                if let Some(stream) = media.downcast_ref::<crate::mpv::MpvStream>() {
                    changes.extend(orientation_settings(stream.orientation()));
                }
                // The volume is kept only if it was changed while the video was up; it is otherwise the one it inherited.
                let audio = imp.controls.audio();
                if audio != imp.audio_at_load.get() {
                    changes.push(("volume", Some(audio.0.into())));
                    changes.push(("muted", audio.1.then(|| true.into())));
                }
                changes
            }
            None if imp.canvas.texture().is_some() => {
                let mut changes = Placement::to_settings(imp.canvas.placement());
                changes.extend(orientation_settings(imp.canvas.orientation()));
                changes
            }
            None => Vec::new(),
        };
        // What is not kept is neither written nor cleared, so it is as it was when it is kept again.
        let remember = imp.remember.get();
        changes.retain(|(key, _)| remember.keeps(key));
        if let Some(mut settings) = settings {
            // Saved on a worker thread, so the write does not hold up the window.
            std::thread::spawn(move || {
                if let Err(err) = settings.apply(changes) {
                    eprintln!("rsimgv: could not save the file's settings: {err}");
                }
            });
        }
        Thumbnails::shared().set_media_playing(false);
        if let Some(media) = media {
            media.set_playing(false);
            // The video is stopped now, rather than when the stream is finally freed.
            if let Some(stream) = media.downcast_ref::<crate::mpv::MpvStream>() {
                stream.stop();
            }
        }
        imp.controls.set_media(None);
        imp.picture.set_paintable(None::<&gdk::Paintable>);
        imp.backdrop.set_texture(None);
        imp.canvas.set_texture(None);
    }

    /// Plays the video shown again from where it was left, so it is made with the current mpv config. Does nothing
    /// when no video is shown.
    pub fn reload_video(&self) {
        let imp = self.imp();
        if imp.media.borrow().is_some() {
            self.load(imp.index.get());
        }
    }

    /// Moves to the item before or after the one shown. Past the first or last item, what happens is up to `folder_end`.
    pub fn step(&self, forward: bool) {
        let imp = self.imp();
        let len = imp.paths.borrow().len();
        let end = imp.folder_end.get();
        match step_index(imp.index.get(), len, forward, end) {
            Some(index) => {
                self.delay_next_video();
                self.load(index);
            }
            None if end == FolderEnd::NextFolder => self.emit_by_name::<()>("folder-end-reached", &[&forward]),
            None => {}
        }
    }

    /// In fullscreen, shows the header when the pointer reaches the top edge, keeps it while the pointer is over
    /// it or within a buffer below it, and hides it a while after the pointer moves away, unless it is pinned.
    fn reveal_header(&self, pointer: Option<(f64, f64)>) {
        let imp = self.imp();
        let header = &*imp.header;
        if !imp.fullscreen.get() || imp.pinned.get() {
            header.set_reveal_child(true);
            return;
        }
        let shown = header.reveals_child();
        let depth = if shown { f64::from(imp.header_bar.height()) + BAR_BUFFER } else { BAR_EDGE_ZONE };
        let near = imp.pinned.get() || pointer.is_some_and(|(_, y)| y < depth);
        if near {
            if let Some(source) = imp.header_hide.take() {
                source.remove();
            }
            header.set_reveal_child(true);
        } else if shown && imp.header_hide.borrow().is_none() {
            let weak = self.downgrade();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(BAR_HIDE_MS), move || {
                let Some(viewer) = weak.upgrade() else { return };
                viewer.imp().header_hide.take();
                if !viewer.imp().pinned.get() {
                    viewer.imp().header.set_reveal_child(false);
                }
            });
            imp.header_hide.replace(Some(source));
        }
    }

    /// Shows the title bar when the pointer is near the top edge and the controls near the bottom, and an
    /// thumbnail bar when the pointer is over its edge. `None` (the pointer left the viewer) hides
    /// them all, except the volume slider keeps the controls open.
    fn reveal_near(&self, pointer: Option<(f64, f64)>) {
        let imp = self.imp();
        imp.pointer.set(pointer);
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let layout = imp.controls_layout.get();
        let bottom = match pointer {
            _ if imp.pinned.get() => true,
            // Near the controls wherever they are, and as before anywhere along the bottom while they are down there.
            Some((x, y)) => self.near_controls(x, y) || (layout.y > 0.66 && y > height * (1.0 - REVEAL_FRACTION)),
            None => false,
        };
        self.reveal_header(pointer);
        // Unlocked, the controls stay up to be dragged.
        imp.controls_revealer.set_reveal_child(bottom || imp.controls.is_volume_open() || !layout.locked);

        if !imp.bar_overlay.get() {
            // Beside the picture, the bar is always shown.
        } else if !imp.bar_shown.get() {
            if let Some(source) = imp.bar_hide.take() {
                source.remove();
            }
            imp.bar_revealer.set_reveal_child(false);
        } else if imp.bar_peek.borrow().is_none() {
            // Hidden, the bar shows when the pointer comes near the edge; shown, it stays while the pointer
            // is over it or within a buffer of it, and hides a while after the pointer moves away.
            let side = imp.bar_side.get().unwrap_or(gtk::PositionType::Bottom);
            let shown = imp.bar_revealer.reveals_child();
            let depth = if shown {
                let (_, natural, _, _) = imp.thumbnail_bar.measure(
                    if matches!(side, gtk::PositionType::Top | gtk::PositionType::Bottom) {
                        gtk::Orientation::Vertical
                    } else {
                        gtk::Orientation::Horizontal
                    },
                    -1,
                );
                f64::from(natural) + BAR_BUFFER
            } else {
                BAR_EDGE_ZONE
            };
            let near = imp.thumbnail_bar.is_dragging()
                || imp.pinned.get()
                || imp.bar_pinned.get()
                || pointer.is_some_and(|(x, y)| match side {
                    gtk::PositionType::Top => y < depth,
                    gtk::PositionType::Bottom => y > height - depth,
                    gtk::PositionType::Left => x < depth,
                    _ => x > width - depth,
                });
            if near {
                if let Some(source) = imp.bar_hide.take() {
                    source.remove();
                }
                imp.bar_revealer.set_reveal_child(true);
            } else if shown && imp.bar_hide.borrow().is_none() {
                let weak = self.downgrade();
                let source = glib::timeout_add_local_once(std::time::Duration::from_millis(BAR_HIDE_MS), move || {
                    let Some(viewer) = weak.upgrade() else { return };
                    viewer.imp().bar_hide.take();
                    viewer.imp().bar_revealer.set_reveal_child(false);
                });
                imp.bar_hide.replace(Some(source));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The pictures either side of the one shown are decoded ahead only while preloading is on.
    pub(crate) fn run_scenarios() {
        let root = std::env::temp_dir().join(format!("rsimgv-viewer-preload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths: Vec<PathBuf> = ["a.png", "b.png", "c.png"].iter().map(|name| root.join(name)).collect();
        for path in &paths {
            let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 8, 8).unwrap();
            pixbuf.savev(path, "png", &[]).unwrap();
        }
        let pump = |until: &dyn Fn() -> bool| {
            let context = glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1000);
            while !until() && std::time::Instant::now() < deadline {
                if !context.iteration(false) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        };

        let viewer: MediaViewer = glib::Object::new();
        let imp = viewer.imp();
        imp.paths.replace(paths);
        viewer.set_preloading(true);
        viewer.prefetch_neighbours(1);
        pump(&|| imp.prefetched.borrow().len() == 2);
        assert_eq!(imp.prefetched.borrow().len(), 2, "both neighbours are decoded");

        viewer.set_preloading(false);
        assert!(imp.prefetched.borrow().is_empty(), "turning it off lets them go");
        viewer.prefetch_neighbours(1);
        pump(&|| false);
        assert!(imp.prefetched.borrow().is_empty(), "nothing is decoded while it is off");

        // A decode under way when it is turned off is not kept.
        viewer.set_preloading(true);
        viewer.prefetch_neighbours(1);
        viewer.set_preloading(false);
        pump(&|| false);
        assert!(imp.prefetched.borrow().is_empty(), "a decode under way is not kept");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn steps_past_the_end_follow_the_setting() {
        assert_eq!(step_index(2, 3, true, FolderEnd::Stop), None);
        assert_eq!(step_index(2, 3, true, FolderEnd::NextFolder), None, "the window moves to the next folder");
        assert_eq!(step_index(2, 3, true, FolderEnd::Loop), Some(0));
        assert_eq!(step_index(0, 3, false, FolderEnd::Loop), Some(2));
        assert_eq!(step_index(1, 3, true, FolderEnd::Loop), Some(2), "looping changes only the ends");
        assert_eq!(step_index(0, 1, true, FolderEnd::Loop), None, "a lone item has nothing to loop to");
        assert_eq!(step_index(0, 0, true, FolderEnd::Loop), None);
        assert_eq!(FolderEnd::from_name("next-folder"), FolderEnd::NextFolder);
        assert_eq!(FolderEnd::from_name("unknown"), FolderEnd::Stop);
    }

    #[test]
    fn neighbours_stop_at_either_end() {
        assert_eq!(neighbour(0, 3, false), None);
        assert_eq!(neighbour(1, 3, false), Some(0));
        assert_eq!(neighbour(1, 3, true), Some(2));
        assert_eq!(neighbour(2, 3, true), None);
        assert_eq!(neighbour(0, 0, true), None);
    }

    #[test]
    fn the_nearest_side_is_measured_as_a_share() {
        assert_eq!(nearest_side(10.0, 300.0, 1000, 600), gtk::PositionType::Left);
        assert_eq!(nearest_side(990.0, 300.0, 1000, 600), gtk::PositionType::Right);
        assert_eq!(nearest_side(500.0, 5.0, 1000, 600), gtk::PositionType::Top);
        assert_eq!(nearest_side(500.0, 590.0, 1000, 600), gtk::PositionType::Bottom);
        // 100px from the left of 1000 is nearer, as a share, than 100px from the bottom of 600.
        assert_eq!(nearest_side(100.0, 500.0, 1000, 600), gtk::PositionType::Left);
    }
}
