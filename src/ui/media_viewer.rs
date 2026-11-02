//! The media viewer: one picture or video filling the window on black, under a header bar with the Back button,
//! the file name and its place in the folder, zoom and fit buttons, fullscreen, and a pin. In fullscreen the
//! header floats over the picture and shows only while the pointer is near the top, unless pinned. A video's control bar floats
//! along the bottom and shows only while the pointer is near it. Left and right step through the pictures
//! and videos of the folder, Escape goes back, and space plays or pauses a video; these keys are bindings,
//! see `keybindings`.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use gtk::glib::prelude::StaticType;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::drag;
use super::image_canvas::{Fit, ImageCanvas, Placement};
use crate::file_settings::FileSettings;
use crate::fs::metadata::MediaInfo;
use crate::fs::thumbnail::Pixels;
use super::media_controls::MediaControls;
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

/// The share of the viewer's height, from its bottom edge, where a video's controls appear.
const REVEAL_FRACTION: f64 = 0.2;

/// How near, in seconds, a video's start or end its position must be to start it from the beginning next time.
const POSITION_MARGIN: f64 = 2.0;

/// The horizontal and vertical alignment of a status overlay at `position`, such as `top-left` or `bottom-middle`.
fn overlay_alignment(position: &str) -> (gtk::Align, gtk::Align) {
    let vertical = if position.starts_with("top") { gtk::Align::Start } else { gtk::Align::End };
    let horizontal = if position.ends_with("left") {
        gtk::Align::Start
    } else if position.ends_with("right") {
        gtk::Align::End
    } else {
        gtk::Align::Center
    };
    (horizontal, vertical)
}

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
        /// Pictures decoded ahead of the item shown, with the paths they are of.
        pub(super) prefetched: RefCell<Vec<(PathBuf, Pixels)>>,
        /// The settings kept for the item shown: a picture's zoom and pan, a video's position and speed. They are
        /// saved when it is left.
        pub(super) file_settings: RefCell<Option<FileSettings>>,
        /// The video shown, if the item is one. The viewer owns it, and the controls drive it.
        pub(super) media: RefCell<Option<gtk::MediaStream>>,
        /// Whether a video starts again when it ends, and whether the next item plays when it ends instead.
        pub(super) repeat: Cell<bool>,
        pub(super) auto_advance: Cell<bool>,
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
        /// Whether the zoom level is shown, and whether the pointer is hidden while it is still.
        pub(super) zoom_indicator: Cell<ZoomIndicator>,
        pub(super) auto_hide_cursor: Cell<bool>,
        /// Hides the pointer once it has been still over the viewer for a while.
        pub(super) cursor_hide: RefCell<Option<glib::SourceId>>,
        /// Whether the status bar under the item is shown.
        pub(super) status_shown: Cell<bool>,
        /// Where the status bar floats over the item in fullscreen, such as `bottom-middle`.
        pub(super) status_overlay_position: RefCell<String>,
        /// The SVP frame interpolation settings, applied to the videos mpv plays.
        pub(super) svp: RefCell<Option<crate::svp::Config>>,
        /// Whether SVP is applied to the video shown.
        pub(super) svp_active: Cell<bool>,
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
                        viewer.imp().zoom_label.set_label(&format!("{:.0}%", scale * 100.0));
                        viewer.update_zoom_indicator();
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
                LazyLock::new(|| vec![Signal::builder("back-requested").build(), Signal::builder("item-changed").param_types([String::static_type()]).build(), Signal::builder("file-dropped").param_types([String::static_type()]).build(), Signal::builder("thumbnail-side-requested").param_types([String::static_type()]).build(), Signal::builder("thumbnail-size-requested").param_types([i32::static_type()]).build()]);
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
                imp.controls.set_navigation(neighbour(index, len, false).is_some(), neighbour(index, len, true).is_some());
                self.refresh_status();
            }
            None if len > 0 => self.load(imp.index.get().min(len - 1)),
            None => {}
        }
    }

    /// Stops any video and drops the items, so the viewer holds nothing once it is hidden.
    pub fn close(&self) {
        self.release();
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

    /// Plays later videos with libmpv, or with GTK's own media backend.
    pub fn set_use_mpv(&self, use_mpv: bool) {
        self.imp().use_mpv.set(use_mpv);
    }

    /// Gives later videos played by mpv the mpv command line options in `options`.
    pub fn set_mpv_options(&self, options: &str) {
        self.imp().mpv_options.replace(options.to_owned());
    }

    /// Changes the volume of the video shown by `delta`.
    pub fn adjust_volume(&self, delta: f64) {
        self.imp().controls.adjust_volume(delta);
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
    pub fn set_scaling(&self, nearest: bool, smooth_upscaling: bool) {
        self.imp().canvas.set_scaling(nearest, smooth_upscaling);
    }

    /// Shows a blurred, dimmed copy of the picture behind it, or not.
    pub fn set_blurred_background(&self, on: bool) {
        self.imp().canvas.set_underlay(on);
    }

    /// Pans the picture shown by `dx` and `dy` pixels.
    pub fn pan(&self, dx: f64, dy: f64) {
        self.imp().canvas.pan(dx, dy);
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
            self.apply_svp(&stream, speed);
        }
    }

    /// Sets the SVP settings, and applies them to the video shown.
    pub fn set_svp(&self, config: crate::svp::Config) {
        self.imp().svp.replace(Some(config));
        let stream = self.imp().media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>().cloned());
        if let Some(stream) = stream {
            let speed = stream.speed().unwrap_or(1.0);
            self.apply_svp(&stream, speed);
        }
    }

    /// Applies SVP to the video `stream` plays at `speed`, once mpv knows the video's frame rate. The video's filter
    /// is only replaced while SVP is on or was on, so videos without it keep their filters.
    fn start_svp(&self, stream: &crate::mpv::MpvStream) {
        let (weak_viewer, weak_stream) = (self.downgrade(), stream.downgrade());
        let mut tries = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (Some(viewer), Some(stream)) = (weak_viewer.upgrade(), weak_stream.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            tries += 1;
            if stream.fps().is_some() {
                let speed = stream.speed().unwrap_or(1.0);
                viewer.apply_svp(&stream, speed);
                return glib::ControlFlow::Break;
            }
            if tries >= 100 {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// Writes the SVP script for `stream`'s video at `speed` and loads it into mpv, or removes the filter when SVP is
    /// off. Only the SVP settings' plugins and the video's own rate are read.
    fn apply_svp(&self, stream: &crate::mpv::MpvStream, speed: f64) {
        let Some(config) = self.imp().svp.borrow().clone() else { return };
        // The OSD says when SVP changes state: on when it is applied, off when it was on and is turned off.
        const OSD_MILLIS: u32 = 2500;
        if !config.enabled {
            let _ = stream.set_video_filter("");
            if self.imp().svp_active.replace(false) {
                stream.show_text("SVP off", OSD_MILLIS);
            }
            return;
        }
        let plugins = crate::svp::find_plugins(&config.plugin_directory);
        let usable = if config.nvof { plugins.svp2.is_some() } else { plugins.svp1.is_some() && plugins.svp2.is_some() };
        let Some(source) = stream.fps() else { return };
        if !usable {
            eprintln!("rsimgv: SVP plugins not found in {}", config.plugin_directory.display());
            stream.show_text(&format!("SVP plugins not found in {}", config.plugin_directory.display()), OSD_MILLIS);
            return;
        }
        let target = crate::svp::target_fps(&config.rate, source, speed, stream.display_fps());
        let directory = crate::svp::script_directory();
        let path = directory.join("video.py");
        let written = std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::write(&path, crate::svp::script(&plugins, source, target, config.nvof)));
        if let Err(err) = written {
            eprintln!("rsimgv: could not write the SVP script: {err}");
            stream.show_text("SVP script could not be written", OSD_MILLIS);
            return;
        }
        println!("rsimgv: SVP script written to {}", path.display());
        match stream.set_video_filter(&crate::svp::filter(&path)) {
            Ok(()) => {
                println!("rsimgv: SVP filter set: {}", crate::svp::filter(&path));
                self.imp().svp_active.set(true);
                let text = format!(
                    "SVP: {} -> {} ({:.0}% speed)",
                    crate::fs::metadata::format_fps(source),
                    crate::fs::metadata::format_fps(target),
                    speed * 100.0
                );
                stream.show_text(&text, OSD_MILLIS);
            }
            Err(err) => {
                eprintln!("rsimgv: SVP could not be applied: {err}");
                stream.show_text("SVP could not be applied", OSD_MILLIS);
            }
        }
    }

    pub fn seek(&self, micros: i64) {
        self.imp().controls.seek_by(micros);
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

    /// The parts of the viewer that float over the item, which KDE's blur is drawn behind: the status overlay and the
    /// header in fullscreen (unless pinned), the video controls, and the thumbnail bar when it floats.
    pub fn floating_widgets(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        let mut widgets = Vec::new();
        if imp.fullscreen.get() {
            if imp.status_bar.is_visible() {
                widgets.push(imp.status_bar.clone().upcast());
            }
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
        widgets
    }

    /// Sets where the status bar floats in fullscreen, such as `top-left` or `bottom-middle`.
    pub fn set_status_overlay_position(&self, position: &str) {
        self.imp().status_overlay_position.replace(position.to_owned());
        self.place_status_bar();
    }

    /// Puts the status bar under the item. In fullscreen it floats over the item instead, at the chosen position,
    /// without its expander, and lets pointer events through so the picture stays usable beneath it.
    fn place_status_bar(&self) {
        let imp = self.imp();
        let bar: &gtk::Box = &imp.status_bar;
        if bar.parent().is_some() {
            bar.unparent();
        }
        if imp.fullscreen.get() {
            let (halign, valign) = overlay_alignment(&imp.status_overlay_position.borrow());
            bar.set_halign(halign);
            bar.set_valign(valign);
            bar.set_margin_start(14);
            bar.set_margin_end(14);
            bar.set_margin_top(14);
            bar.set_margin_bottom(14);
            bar.set_can_target(false);
            bar.add_css_class("status-overlay");
            imp.status_expander.set_visible(false);
            imp.overlay.add_overlay(bar);
        } else {
            bar.set_halign(gtk::Align::Fill);
            bar.set_valign(gtk::Align::Fill);
            bar.set_margin_start(12);
            bar.set_margin_end(12);
            bar.set_margin_top(4);
            bar.set_margin_bottom(4);
            bar.set_can_target(true);
            bar.remove_css_class("status-overlay");
            imp.status_expander.set_visible(true);
            self.append(bar);
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
        if !imp.status_shown.get() {
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

    /// Shows the item at `index`, replacing the one before it.
    fn load(&self, index: usize) {
        let imp = self.imp();
        let Some(path) = imp.paths.borrow().get(index).cloned() else { return };
        let len = imp.paths.borrow().len();
        self.release();
        imp.index.set(index);
        imp.thumbnail_bar.select(index);

        let name = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        imp.filename.set_label(&name);
        imp.position.set_label(&format!("{} / {}", index + 1, len));
        self.refresh_status();
        imp.controls.set_navigation(neighbour(index, len, false).is_some(), neighbour(index, len, true).is_some());

        // The file's settings are read on a worker thread: the moviehash reads the file, and the settings file is parsed.
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let settings = gio::spawn_blocking({
                let path = path.clone();
                move || FileSettings::for_path(&path).ok()
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
        if video {
            // libmpv plays the video if it is chosen and starts; otherwise GTK's media backend does.
            let media: gtk::MediaStream = imp
                .use_mpv
                .get()
                .then(|| crate::mpv::MpvStream::for_path(&path, &imp.mpv_options.borrow()))
                .flatten()
                .map(|stream| {
                    // Each video plays at the speed it was left at, or normal speed.
                    let speed = settings.as_ref().and_then(|settings| settings.get_f64("speed")).unwrap_or(1.0);
                    stream.set_speed(speed);
                    self.start_svp(&stream);
                    // Frames are rendered at device pixels, so the video and its OSD are drawn at the display's scale.
                    stream.set_scale(imp.picture.scale_factor());
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
            media.connect_error_notify(|media| {
                if let Some(err) = media.error() {
                    eprintln!("rsimgv: {err}");
                }
            });
            imp.pages.set_visible_child(&*imp.picture);
            // mpv fits the video to the area it is given, so that area is the whole picture, and its OSD sits in its corner.
            let fit = if media.is::<crate::mpv::MpvStream>() { gtk::ContentFit::Fill } else { gtk::ContentFit::Contain };
            imp.picture.set_content_fit(fit);
            imp.picture.set_paintable(Some(&media));
            imp.controls.set_media(Some(&media));
            // Playing is requested once the stream is ready, since a request made before then is dropped. It starts
            // where it was left.
            let position = settings.as_ref().and_then(|settings| settings.get_f64("position"));
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
        } else {
            imp.pages.set_visible_child(&*imp.canvas);
            let placement = settings.as_ref().and_then(Placement::from_settings);
            self.show_picture(index, &path, placement);
            self.prefetch_neighbours(index);
        }
        imp.file_settings.replace(settings);
        imp.controls_revealer.set_visible(video);
        self.update_zoom_indicator();
        self.emit_by_name::<()>("item-changed", &[&path.to_string_lossy().into_owned()]);
    }

    /// Shows the picture at `path`, the item at `index`, once it is decoded. A picture decoded ahead is shown at once. A
    /// decode that finishes after another item is shown is not shown.
    fn show_picture(&self, index: usize, path: &Path, placement: Option<Placement>) {
        let imp = self.imp();
        let ahead = {
            let mut prefetched = imp.prefetched.borrow_mut();
            prefetched.iter().position(|(ahead, _)| ahead == path).map(|position| prefetched.remove(position).1)
        };
        if let Some(pixels) = ahead {
            self.set_picture(&pixels, placement);
            return;
        }
        let weak = self.downgrade();
        let path = path.to_path_buf();
        glib::spawn_future_local(async move {
            let Some(pixels) = decode_picture(path).await else { return };
            let Some(viewer) = weak.upgrade() else { return };
            if viewer.imp().index.get() == index && viewer.imp().media.borrow().is_none() {
                viewer.set_picture(&pixels, placement);
            }
        });
    }

    /// Puts the decoded picture on the canvas, at the placement it was left at.
    fn set_picture(&self, pixels: &Pixels, placement: Option<Placement>) {
        let imp = self.imp();
        imp.canvas.set_texture(Some(pixels.texture()));
        if let Some(placement) = placement {
            imp.canvas.restore(placement);
        }
        self.update_zoom_indicator();
    }

    /// Decodes the pictures either side of the item at `index`, so that stepping to them shows them at once.
    fn prefetch_neighbours(&self, index: usize) {
        let imp = self.imp();
        let paths = imp.paths.borrow();
        let len = paths.len();
        let ahead: Vec<PathBuf> = [neighbour(index, len, false), neighbour(index, len, true)]
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
        let settings = imp.file_settings.take();
        let media = imp.media.take();
        let changes = match &media {
            Some(media) => video_settings(media),
            None if imp.canvas.texture().is_some() => Placement::to_settings(imp.canvas.placement()),
            None => Vec::new(),
        };
        if let Some(mut settings) = settings {
            // Saved on a worker thread, so the write does not hold up the window.
            std::thread::spawn(move || {
                if let Err(err) = settings.apply(changes) {
                    eprintln!("rsimgv: could not save the file's settings: {err}");
                }
            });
        }
        if let Some(media) = media {
            media.set_playing(false);
            // The video is stopped now, rather than when the stream is finally freed.
            if let Some(stream) = media.downcast_ref::<crate::mpv::MpvStream>() {
                stream.stop();
            }
        }
        imp.controls.set_media(None);
        imp.picture.set_paintable(None::<&gdk::Paintable>);
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

    /// Moves to the item before or after the one shown, if there is one.
    pub fn step(&self, forward: bool) {
        let imp = self.imp();
        let len = imp.paths.borrow().len();
        if let Some(index) = neighbour(imp.index.get(), len, forward) {
            self.load(index);
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
        let bottom = match pointer {
            _ if imp.pinned.get() => true,
            Some((_, y)) => y > height * (1.0 - REVEAL_FRACTION),
            None => false,
        };
        self.reveal_header(pointer);
        imp.controls_revealer.set_reveal_child(bottom || imp.controls.is_volume_open());

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
mod tests {
    use super::*;

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
