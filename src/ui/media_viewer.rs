//! The media viewer: one picture or video filling the window on black. The Back button and the file name
//! sit at the top left and show only while the pointer is near the top. A video's control bar floats
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

use super::image_canvas::{Fit, ImageCanvas};
use super::media_controls::MediaControls;
use super::thumbnail_bar::ThumbnailBar;

/// How much a volume key changes the volume, as a fraction of full.
pub const VOLUME_STEP: f64 = 0.05;

/// The share of the viewer's height, from its top or bottom edge, where the bars appear.
const REVEAL_FRACTION: f64 = 0.2;

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

/// The least depth, in pixels, of the edge zone that shows an overlay thumbnail bar.
const BAR_EDGE_MIN: i32 = 48;

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
        pub header: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub controls_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub controls: TemplateChild<MediaControls>,
        #[template_child]
        pub overlay: TemplateChild<gtk::Overlay>,
        /// The thumbnails of the items, beside the picture.
        pub(super) thumbnail_bar: ThumbnailBar,
        /// Holds the thumbnail bar over the picture when it is an overlay.
        pub(super) bar_revealer: gtk::Revealer,
        /// The side of the picture the bar is on (bottom for `None`), whether it shows, and whether it
        /// floats over the picture, appearing only while the pointer is near its edge.
        pub(super) bar_side: Cell<Option<gtk::PositionType>>,
        pub(super) bar_shown: Cell<bool>,
        pub(super) bar_overlay: Cell<bool>,
        /// The folder's pictures and videos, in display order, and the index of the one shown.
        pub(super) paths: RefCell<Vec<PathBuf>>,
        pub(super) index: Cell<usize>,
        /// The video shown, if the item is one. The viewer owns it, and the controls drive it.
        pub(super) media: RefCell<Option<gtk::MediaFile>>,
        /// Whether a video starts again when it ends, and whether the next item plays when it ends instead.
        pub(super) repeat: Cell<bool>,
        pub(super) auto_advance: Cell<bool>,
        /// Whether the controls and the title stay shown, rather than hiding until the pointer comes near.
        pub(super) pinned: Cell<bool>,
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

            self.bar_revealer.set_transition_duration(150);
            self.overlay.add_overlay(&self.bar_revealer);
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
            self.thumbnail_bar.connect_item_activated({
                let weak = weak.clone();
                move |_, index| {
                    if let Some(viewer) = weak.upgrade() {
                        viewer.load(index);
                    }
                }
            });
            obj.set_thumbnail_bar(gtk::PositionType::Bottom, true, false);

        }

        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> =
                LazyLock::new(|| vec![Signal::builder("back-requested").build(), Signal::builder("item-changed").param_types([String::static_type()]).build(), Signal::builder("thumbnail-side-requested").param_types([String::static_type()]).build()]);
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

    /// Puts the thumbnail bar on the `side` of the picture, shown or hidden. As an overlay it floats over
    /// the picture and appears only while the pointer is near that edge; otherwise it sits beside it.
    pub fn set_thumbnail_bar(&self, side: gtk::PositionType, shown: bool, overlay: bool) {
        let imp = self.imp();
        imp.bar_side.set(Some(side));
        imp.bar_shown.set(shown);
        imp.bar_overlay.set(overlay);
        let bar = &imp.thumbnail_bar;
        let revealer = &imp.bar_revealer;

        // Take the bar out of wherever it is, then put it where it now goes.
        if bar.parent().as_ref() == Some(self.upcast_ref::<gtk::Widget>()) {
            self.remove(bar);
        } else if revealer.child().is_some() {
            revealer.set_child(None::<&gtk::Widget>);
        }
        let along = match side {
            gtk::PositionType::Top | gtk::PositionType::Bottom => gtk::Orientation::Horizontal,
            _ => gtk::Orientation::Vertical,
        };
        bar.set_orientation(along);
        bar.set_visible(shown);
        revealer.set_visible(shown && overlay);

        if overlay {
            let (halign, valign, transition) = match side {
                gtk::PositionType::Top => (gtk::Align::Fill, gtk::Align::Start, gtk::RevealerTransitionType::SlideDown),
                gtk::PositionType::Bottom => (gtk::Align::Fill, gtk::Align::End, gtk::RevealerTransitionType::SlideUp),
                gtk::PositionType::Left => (gtk::Align::Start, gtk::Align::Fill, gtk::RevealerTransitionType::SlideRight),
                _ => (gtk::Align::End, gtk::Align::Fill, gtk::RevealerTransitionType::SlideLeft),
            };
            revealer.set_halign(halign);
            revealer.set_valign(valign);
            revealer.set_transition_type(transition);
            revealer.set_reveal_child(false);
            revealer.set_child(Some(bar));
            return;
        }

        // The viewer box runs across the bar: a row along the top or bottom, a column beside.
        self.set_orientation(if along == gtk::Orientation::Horizontal {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        if matches!(side, gtk::PositionType::Top | gtk::PositionType::Left) {
            self.prepend(bar);
        } else {
            self.append(bar);
        }
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
        imp.pinned.set(!imp.pinned.get());
        self.reveal_near(None);
    }

    /// Changes the volume of the video shown by `delta`.
    pub fn adjust_volume(&self, delta: f64) {
        self.imp().controls.adjust_volume(delta);
    }

    /// Zooms the picture shown in, or out.
    pub fn zoom_in(&self) {
        self.imp().canvas.zoom_in();
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
        imp.controls.set_navigation(neighbour(index, len, false).is_some(), neighbour(index, len, true).is_some());

        let video = is_video(&path);
        if video {
            let media = gtk::MediaFile::for_filename(&path);
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
            // Playing is requested once the stream is ready, since a request made before then is dropped.
            media.connect_prepared_notify(|media| {
                if media.is_prepared() {
                    media.play();
                }
            });
            imp.media.replace(Some(media));
        } else {
            imp.pages.set_visible_child(&*imp.canvas);
            imp.canvas.set_texture(load_texture(&path));
        }
        imp.controls_revealer.set_visible(video);
        self.emit_by_name::<()>("item-changed", &[&path.to_string_lossy().into_owned()]);
    }

    /// Clears the shown item: a video is paused and released, and the picture is emptied.
    fn release(&self) {
        let imp = self.imp();
        if let Some(media) = imp.media.take() {
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

    /// Shows the title bar when the pointer is near the top edge and the controls near the bottom, and an
    /// overlay thumbnail bar when the pointer is over its edge. `None` (the pointer left the viewer) hides
    /// them all, except the volume slider keeps the controls open.
    fn reveal_near(&self, pointer: Option<(f64, f64)>) {
        let imp = self.imp();
        let (width, height) = (f64::from(self.width()), f64::from(self.height()));
        let (top, bottom) = match pointer {
            _ if imp.pinned.get() => (true, true),
            Some((_, y)) => (y < height * REVEAL_FRACTION, y > height * (1.0 - REVEAL_FRACTION)),
            None => (false, false),
        };
        imp.header.set_reveal_child(top);
        imp.controls_revealer.set_reveal_child(bottom || imp.controls.is_volume_open());

        if imp.bar_overlay.get() && imp.bar_shown.get() {
            // The bar's edge zone is as deep as the bar, so the pointer keeps it open while over it.
            let side = imp.bar_side.get().unwrap_or(gtk::PositionType::Bottom);
            let (_, natural, _, _) = imp.thumbnail_bar.measure(
                if matches!(side, gtk::PositionType::Top | gtk::PositionType::Bottom) {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                },
                -1,
            );
            let depth = f64::from(natural.max(BAR_EDGE_MIN));
            let near = imp.thumbnail_bar.is_dragging()
                || pointer.is_some_and(|(x, y)| match side {
                    gtk::PositionType::Top => y < depth,
                    gtk::PositionType::Bottom => y > height - depth,
                    gtk::PositionType::Left => x < depth,
                    _ => x > width - depth,
                });
            imp.bar_revealer.set_reveal_child(near || imp.pinned.get());
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
