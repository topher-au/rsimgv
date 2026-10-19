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

use super::image_canvas::{Fit, ImageCanvas, Placement};
use crate::file_settings::FileSettings;
use super::media_controls::MediaControls;
use super::thumbnail_bar::ThumbnailBar;

/// How much a volume key changes the volume, as a fraction of full.
pub const VOLUME_STEP: f64 = 0.05;

/// The share of the viewer's height, from its bottom edge, where a video's controls appear.
const REVEAL_FRACTION: f64 = 0.2;

/// How near, in seconds, a video's start or end its position must be to start it from the beginning next time.
const POSITION_MARGIN: f64 = 2.0;

/// The picture in `path`, or `None` if it cannot be read.
fn load_texture(path: &Path) -> Option<gdk::Texture> {
    match gdk::Texture::from_file(&gio::File::for_path(path)) {
        Ok(texture) => Some(texture),
        Err(err) => {
            eprintln!("rsimgv: could not open {}: {err}", path.display());
            None
        }
    }
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

            self.back_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.request_back();
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
                LazyLock::new(|| vec![Signal::builder("back-requested").build(), Signal::builder("item-changed").param_types([String::static_type()]).build(), Signal::builder("thumbnail-side-requested").param_types([String::static_type()]).build(), Signal::builder("thumbnail-size-requested").param_types([i32::static_type()]).build()]);
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

    /// Stops any video and drops the items, so the viewer holds nothing once it is hidden.
    pub fn close(&self) {
        self.release();
        self.imp().paths.take();
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
    }

    /// Keeps the controls and the title shown, or lets them hide again.
    pub fn toggle_pinned(&self) {
        let imp = self.imp();
        imp.pin_button.set_active(!imp.pinned.get());
    }

    /// Notes whether the window is fullscreen, which decides where the header goes.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.imp().fullscreen.set(fullscreen);
        self.place_header();
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

    /// Limits how much fitting to the window enlarges a small picture, to `factor` times its size; 0 has no limit.
    pub fn set_expand_limit(&self, factor: i32) {
        self.imp().canvas.set_expand_limit(f64::from(factor));
    }

    /// Whether a new picture keeps the fit mode last chosen, rather than fitting the window.
    pub fn set_keep_fit(&self, keep: bool) {
        self.imp().canvas.set_keep_fit(keep);
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
        if let Some(stream) = imp.media.borrow().as_ref().and_then(|media| media.downcast_ref::<crate::mpv::MpvStream>()) {
            stream.set_speed(speed);
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

    fn request_back(&self) {
        self.emit_by_name::<()>("back-requested", &[]);
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
        imp.controls.set_navigation(neighbour(index, len, false).is_some(), neighbour(index, len, true).is_some());

        let settings = FileSettings::for_path(&path).ok();
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
                    stream.set_speed(settings.as_ref().and_then(|settings| settings.get_f64("speed")).unwrap_or(1.0));
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
            imp.canvas.set_texture(load_texture(&path));
            if let Some(placement) = settings.as_ref().and_then(Placement::from_settings) {
                imp.canvas.restore(placement);
            }
        }
        imp.file_settings.replace(settings);
        imp.controls_revealer.set_visible(video);
        imp.zoom_box.set_visible(!video);
        self.emit_by_name::<()>("item-changed", &[&path.to_string_lossy().into_owned()]);
    }

    /// Clears the shown item: a video is paused and released, and the picture is emptied.
    fn release(&self) {
        let imp = self.imp();
        let mut settings = imp.file_settings.take();
        let media = imp.media.take();
        let changes = match &media {
            Some(media) => video_settings(media),
            None if imp.canvas.texture().is_some() => Placement::to_settings(imp.canvas.placement()),
            None => Vec::new(),
        };
        if let Some(settings) = settings.as_mut() {
            if let Err(err) = settings.apply(changes) {
                eprintln!("rsimgv: could not save the file's settings: {err}");
            }
        }
        if let Some(media) = media {
            media.set_playing(false);
        }
        imp.controls.set_media(None);
        imp.picture.set_paintable(None::<&gdk::Paintable>);
        imp.canvas.set_texture(None);
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
