//! The picture view for images. The picture is drawn at the size its fit mode gives, or at a zoom the user chose,
//! centred in the view and panned by an offset. Zooming and panning ease to their new place over a few frames. A
//! blurred, dimmed copy of the picture can fill the view behind it.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// How much one zoom step changes the scale.
const DEFAULT_ZOOM_STEP: f64 = 1.25;
/// The smallest and largest scale the zoom reaches, relative to the picture's own size.
const MIN_SCALE: f64 = 0.05;
const MAX_SCALE: f64 = 20.0;
/// How far a pan action moves the picture, in pixels.
pub const PAN_STEP: f64 = 40.0;
/// How quickly the picture eases to a new zoom or pan, as a rate per second: about 90% of the way in a quarter
/// of a second.
const EASE_RATE: f64 = 10.0;
/// The radius of the blur behind the picture, in pixels.
const BLUR_RADIUS: f64 = 40.0;
/// How dark the blurred background is: the black laid over it.
const DIM: gdk::RGBA = gdk::RGBA::new(0.0, 0.0, 0.0, 0.55);

/// A fit mode: the picture is scaled to fit the view, its width, its height, or shown at its own size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fit {
    #[default]
    Window,
    Width,
    Height,
    Original,
}

impl Fit {
    /// The name the mode is saved under.
    pub fn name(self) -> &'static str {
        match self {
            Fit::Window => "window",
            Fit::Width => "width",
            Fit::Height => "height",
            Fit::Original => "original",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Fit::Window, Fit::Width, Fit::Height, Fit::Original].into_iter().find(|fit| fit.name() == name)
    }

    /// The fit mode after this one, for cycling through them.
    pub fn next(self) -> Fit {
        match self {
            Fit::Window => Fit::Width,
            Fit::Width => Fit::Height,
            Fit::Height => Fit::Original,
            Fit::Original => Fit::Window,
        }
    }
}

/// How the picture is drawn: its scale, and its offset from the centre of the view.
/// How the user left a picture: its fit mode, or `None` for its zoom, and its pan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    fit: Option<Fit>,
    zoom: f64,
    offset: (f64, f64),
}

impl Placement {
    /// The keys a placement is saved under in a file's settings.
    const KEYS: [&'static str; 4] = ["fit", "zoom", "pan-x", "pan-y"];

    /// The settings changes that save `placement`, or that clear a saved one for `None`.
    pub fn to_settings(placement: Option<Placement>) -> Vec<(&'static str, Option<serde_yaml_ng::Value>)> {
        let Some(placement) = placement else { return Self::KEYS.iter().map(|key| (*key, None)).collect() };
        vec![
            ("fit", placement.fit.map(|fit| fit.name().into())),
            ("zoom", Some(placement.zoom.into())),
            ("pan-x", Some(placement.offset.0.into())),
            ("pan-y", Some(placement.offset.1.into())),
        ]
    }

    /// The placement saved in `settings`, if there is one.
    pub fn from_settings(settings: &crate::file_settings::FileSettings) -> Option<Placement> {
        let zoom = settings.get_f64("zoom")?;
        Some(Placement {
            fit: settings.get_str("fit").and_then(Fit::from_name),
            zoom,
            offset: (settings.get_f64("pan-x").unwrap_or(0.0), settings.get_f64("pan-y").unwrap_or(0.0)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct View {
    scale: f64,
    offset: (f64, f64),
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ImageCanvas {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        /// The fit mode, or `None` once the user has zoomed.
        pub(super) fit: Cell<Option<Fit>>,
        /// The fit mode last chosen, which a new picture gets, and whether it does rather than fitting the window.
        pub(super) last_fit: Cell<Option<Fit>>,
        pub(super) keep_fit: Cell<bool>,
        /// How much one zoom step changes the scale, as a factor.
        pub(super) zoom_step: Cell<f64>,
        /// The most fitting to the window enlarges a picture smaller than the view, as a multiple of its size; 0 has no limit.
        pub(super) expand_limit: Cell<f64>,
        /// The fit mode a picture gets when it is opened and no other is kept.
        pub(super) default_fit: Cell<Fit>,
        /// The scale used while no fit mode is set.
        pub(super) zoom: Cell<f64>,
        /// The offset the picture is panned to: where it is heading.
        pub(super) offset: Cell<(f64, f64)>,
        /// The scale last reported through `scale-changed`.
        pub(super) reported_scale: Cell<f64>,
        /// Where the pointer is over the canvas, or `None` while it is elsewhere.
        pub(super) pointer: Cell<Option<(f64, f64)>>,
        /// The offset when the current drag began, so a drag moves from there.
        pub(super) drag_start: Cell<(f64, f64)>,
        /// How the picture is drawn right now. It eases toward the target view.
        pub(super) shown: Cell<Option<View>>,
        /// The frame callback that eases `shown` toward the target, while it runs.
        pub(super) easing: RefCell<Option<gtk::TickCallbackId>>,
        /// The frame time of the last eased frame, in microseconds.
        pub(super) last_frame: Cell<i64>,
        /// Whether a blurred, dimmed, cropped copy of the picture fills the view behind it.
        pub(super) underlay: Cell<bool>,
        /// Whether pictures are drawn with nearest-neighbour sampling, rather than linear.
        pub(super) nearest_scaling: Cell<bool>,
        /// Whether enlarged pictures are drawn with linear sampling too, rather than nearest.
        pub(super) smooth_upscaling: Cell<bool>,
        /// Whether zooming and panning ease to their new place, rather than jumping there.
        pub(super) smooth_scrolling: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImageCanvas {
        const NAME: &'static str = "RsimgvImageCanvas";
        type Type = super::ImageCanvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ImageCanvas {
        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> = LazyLock::new(|| {
                vec![glib::subclass::Signal::builder("scale-changed").param_types([f64::static_type()]).build()]
            });
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.fit.set(Some(Fit::Window));
            self.keep_fit.set(true);
            self.smooth_upscaling.set(true);
            self.smooth_scrolling.set(true);
            self.zoom_step.set(DEFAULT_ZOOM_STEP);
            // A zoomed or panned picture is drawn past the canvas, so clip it to keep it off its neighbours.
            self.obj().set_overflow(gtk::Overflow::Hidden);
            self.zoom.set(1.0);
            let obj = self.obj();
            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin({
                let weak = obj.downgrade();
                move |_, _, _| {
                    if let Some(canvas) = weak.upgrade() {
                        canvas.imp().drag_start.set(canvas.imp().offset.get());
                    }
                }
            });
            drag.connect_drag_update({
                let weak = obj.downgrade();
                move |_, dx, dy| {
                    if let Some(canvas) = weak.upgrade() {
                        let (x, y) = canvas.imp().drag_start.get();
                        // A drag follows the pointer exactly, so it is not eased.
                        canvas.imp().offset.set((x + dx, y + dy));
                        canvas.snap();
                    }
                }
            });
            obj.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion({
                let weak = obj.downgrade();
                move |_, x, y| {
                    if let Some(canvas) = weak.upgrade() {
                        canvas.imp().pointer.set(Some((x, y)));
                    }
                }
            });
            motion.connect_leave({
                let weak = obj.downgrade();
                move |_| {
                    if let Some(canvas) = weak.upgrade() {
                        canvas.imp().pointer.set(None);
                    }
                }
            });
            obj.add_controller(motion);

            // Ctrl and the wheel zoom on the pointer; a smooth scroll zooms by as much as it moves.
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll({
                let weak = obj.downgrade();
                move |controller, _, dy| {
                    let Some(canvas) = weak.upgrade() else { return glib::Propagation::Proceed };
                    if !controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                        return glib::Propagation::Proceed;
                    }
                    canvas.zoom_at_pointer(canvas.imp().zoom_step.get().powf(-dy.clamp(-1.0, 1.0)));
                    glib::Propagation::Stop
                }
            });
            obj.add_controller(scroll);
        }
    }

    impl WidgetImpl for ImageCanvas {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let canvas = self.obj();
            let Some(texture) = self.texture.borrow().clone() else { return };
            if self.underlay.get() {
                canvas.draw_underlay(snapshot, &texture);
            }
            let Some(target) = canvas.target_view() else { return };
            // The scale the picture heads to changes with zooms, fits and the view's size; it is reported
            // after the frame, as signals should not run while drawing.
            if (self.reported_scale.get() - target.scale).abs() > f64::EPSILON {
                self.reported_scale.set(target.scale);
                let weak = canvas.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(canvas) = weak.upgrade() {
                        canvas.emit_by_name::<()>("scale-changed", &[&canvas.imp().reported_scale.get()]);
                    }
                });
            }
            // Without smooth scrolling the view is never behind the target, so nothing eases.
            let view = match self.shown.get() {
                Some(view) if self.smooth_scrolling.get() => view,
                _ => {
                    self.shown.set(Some(target));
                    target
                }
            };
            // A change in the view's size changes the target, so the picture eases to it.
            if view != target {
                canvas.ease();
            }
            let Some((x, y, width, height)) = canvas.placed(view) else { return };
            let filter = canvas.scaling_filter(view.scale);
            snapshot.append_scaled_texture(&texture, filter, &graphene::Rect::new(x as f32, y as f32, width as f32, height as f32));
        }
    }
}

glib::wrapper! {
    /// A picture drawn to fit or zoomed within its view, and panned by dragging or by `pan`.
    pub struct ImageCanvas(ObjectSubclass<imp::ImageCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ImageCanvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ImageCanvas {
    /// Shows `texture`, or nothing. The picture is centred again in the fit mode, dropping any zoom, without
    /// easing from the last one.
    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        let imp = self.imp();
        let default = imp.default_fit.get();
        let fit = if imp.keep_fit.get() { imp.last_fit.get().unwrap_or(default) } else { default };
        imp.fit.set(Some(fit));
        imp.texture.replace(texture);
        self.imp().offset.set((0.0, 0.0));
        self.snap();
    }

    /// Fits the picture to the view in `fit` mode, centred again.
    pub fn set_fit(&self, fit: Fit) {
        self.imp().fit.set(Some(fit));
        self.imp().last_fit.set(Some(fit));
        self.imp().offset.set((0.0, 0.0));
        self.ease();
    }

    /// Moves to the fit mode after the current one. From a zoom, that is the first fit mode.
    pub fn cycle_fit(&self) {
        let next = self.imp().fit.get().map_or(Fit::Window, Fit::next);
        self.set_fit(next);
    }

    pub fn zoom_in(&self) {
        self.zoom_by(self.imp().zoom_step.get());
    }

    pub fn zoom_out(&self) {
        self.zoom_by(1.0 / self.imp().zoom_step.get());
    }

    /// Pans the picture by `dx` and `dy` pixels, easing there, as far as it can go while still covering the view.
    pub fn pan(&self, dx: f64, dy: f64) {
        let (x, y) = self.imp().offset.get();
        self.imp().offset.set((x + dx, y + dy));
        self.ease();
    }

    /// Whether the user has zoomed the picture, rather than it being in a fit mode.
    pub fn is_zoomed(&self) -> bool {
        self.imp().fit.get().is_none()
    }

    /// How the picture is zoomed and panned, if the user has zoomed or panned it.
    pub fn placement(&self) -> Option<Placement> {
        let imp = self.imp();
        imp.texture.borrow().as_ref()?;
        let offset = self.target_view().map_or(imp.offset.get(), |view| view.offset);
        let moved = offset.0.abs() >= 0.5 || offset.1.abs() >= 0.5;
        (imp.fit.get().is_none() || moved).then(|| Placement { fit: imp.fit.get(), zoom: imp.zoom.get(), offset })
    }

    /// Zooms and pans the picture as `placement` says, without easing.
    pub fn restore(&self, placement: Placement) {
        let imp = self.imp();
        imp.fit.set(placement.fit);
        imp.zoom.set(placement.zoom);
        imp.offset.set(placement.offset);
        self.snap();
    }

    /// The picture shown, at its own size, if there is one.
    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    /// Calls `callback` with the scale the picture is drawn at, relative to its own size, when it changes.
    pub fn connect_scale_changed<F: Fn(&Self, f64) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("scale-changed", false, move |values| {
            let canvas = values[0].get::<ImageCanvas>().expect("canvas argument");
            callback(&canvas, values[1].get::<f64>().expect("scale argument"));
            None
        })
    }

    /// Makes each zoom step change the scale by `percent`.
    pub fn set_zoom_step(&self, percent: i32) {
        self.imp().zoom_step.set(1.0 + f64::from(percent.max(1)) / 100.0);
    }

    /// Limits how much fitting to the window enlarges a small picture, to `factor` times its size; 0 has no limit.
    pub fn set_expand_limit(&self, factor: f64) {
        self.imp().expand_limit.set(factor);
        self.ease();
    }

    /// Sets the fit mode a new picture gets, when it is not kept from the last one.
    pub fn set_default_fit(&self, fit: Fit) {
        self.imp().default_fit.set(fit);
    }

    /// Whether zooming and panning ease to their new place over a few frames, rather than jumping there.
    pub fn set_smooth_scrolling(&self, on: bool) {
        self.imp().smooth_scrolling.set(on);
    }

    /// Sets how pictures are resampled: nearest keeps their pixels sharp. Enlarged pictures are drawn nearest too
    /// when `smooth_upscaling` is off.
    pub fn set_scaling(&self, nearest: bool, smooth_upscaling: bool) {
        self.imp().nearest_scaling.set(nearest);
        self.imp().smooth_upscaling.set(smooth_upscaling);
        self.queue_draw();
    }

    /// Whether a new picture keeps the fit mode last chosen, rather than fitting the window.
    pub fn set_keep_fit(&self, keep: bool) {
        self.imp().keep_fit.set(keep);
    }

    /// Draws a blurred, dimmed copy of the picture, cropped to fill the view, behind the picture. Off by default.
    pub fn set_underlay(&self, on: bool) {
        self.imp().underlay.set(on);
        self.queue_draw();
    }

    /// Zooms in a step, keeping the point under the pointer in place, or the centre if the pointer is elsewhere.
    pub fn zoom_in_at_pointer(&self) {
        self.zoom_at_pointer(self.imp().zoom_step.get());
    }

    pub fn zoom_out_at_pointer(&self) {
        self.zoom_at_pointer(1.0 / self.imp().zoom_step.get());
    }

    /// Zooms by `factor`, keeping the point of the picture under the pointer where it is.
    fn zoom_at_pointer(&self, factor: f64) {
        let Some(view) = self.target_view() else { return };
        let Some((px, py)) = self.imp().pointer.get() else {
            self.zoom_by(factor);
            return;
        };
        let scale = (view.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = scale / view.scale;
        // The pointer's place relative to the view's centre, less the offset, is the point on the picture,
        // which moves with the scale.
        let (view_width, view_height) = self.view_size();
        let (cx, cy) = (px - view_width / 2.0, py - view_height / 2.0);
        let offset = (cx - (cx - view.offset.0) * ratio, cy - (cy - view.offset.1) * ratio);
        let imp = self.imp();
        imp.zoom.set(scale);
        imp.fit.set(None);
        imp.offset.set(offset);
        self.ease();
    }

    fn zoom_by(&self, factor: f64) {
        let Some(scale) = self.current_scale() else { return };
        self.imp().zoom.set((scale * factor).clamp(MIN_SCALE, MAX_SCALE));
        self.imp().fit.set(None);
        self.ease();
    }

    /// Starts easing the shown view toward the target, if it is not already doing so. Without smooth scrolling
    /// the view jumps there.
    fn ease(&self) {
        if !self.imp().smooth_scrolling.get() {
            self.snap();
            return;
        }
        if self.imp().easing.borrow().is_some() {
            return;
        }
        self.imp().last_frame.set(0);
        let id = self.add_tick_callback(|canvas, clock| canvas.ease_step(clock.frame_time()));
        self.imp().easing.replace(Some(id));
    }

    /// Moves the shown view a frame closer to the target. Stops once it is there.
    fn ease_step(&self, now: i64) -> glib::ControlFlow {
        let last = self.imp().last_frame.replace(now);
        let seconds = if last == 0 { 1.0 / 60.0 } else { (now - last) as f64 / 1_000_000.0 };
        let Some(target) = self.target_view() else {
            self.imp().easing.take();
            return glib::ControlFlow::Break;
        };
        let current = self.imp().shown.get().unwrap_or(target);
        let factor = 1.0 - (-seconds * EASE_RATE).exp();
        let next = View {
            scale: current.scale + (target.scale - current.scale) * factor,
            offset: (
                current.offset.0 + (target.offset.0 - current.offset.0) * factor,
                current.offset.1 + (target.offset.1 - current.offset.1) * factor,
            ),
        };
        let settled = (next.scale - target.scale).abs() < target.scale * 1e-4
            && (next.offset.0 - target.offset.0).abs() < 0.5
            && (next.offset.1 - target.offset.1).abs() < 0.5;
        let view = if settled { target } else { next };
        self.imp().shown.set(Some(view));
        self.queue_draw();
        if settled {
            self.imp().easing.take();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    }

    /// Jumps the shown view to the target, with no easing.
    fn snap(&self) {
        if let Some(target) = self.target_view() {
            self.imp().shown.set(Some(target));
        }
        self.queue_draw();
    }

    fn texture_size(&self) -> Option<(f64, f64)> {
        let texture = self.imp().texture.borrow().clone()?;
        let (width, height) = (f64::from(texture.width()), f64::from(texture.height()));
        (width > 0.0 && height > 0.0).then_some((width, height))
    }

    fn view_size(&self) -> (f64, f64) {
        (f64::from(self.width()), f64::from(self.height()))
    }

    /// The scale the picture is heading to, or `None` while there is no picture or the view has no size yet.
    fn current_scale(&self) -> Option<f64> {
        let (texture_width, texture_height) = self.texture_size()?;
        let (view_width, view_height) = self.view_size();
        if view_width <= 0.0 || view_height <= 0.0 {
            return None;
        }
        let fitted = |scale: f64| {
            let limit = self.imp().expand_limit.get();
            if limit > 0.0 { scale.min(limit) } else { scale }
        };
        Some(match self.imp().fit.get() {
            Some(Fit::Window) => fitted((view_width / texture_width).min(view_height / texture_height)),
            Some(Fit::Width) => view_width / texture_width,
            Some(Fit::Height) => view_height / texture_height,
            Some(Fit::Original) => 1.0,
            None => self.imp().zoom.get(),
        })
    }

    /// How far a picture drawn at `scale` may be panned from the centre in each direction: half of what it overhangs
    /// the view.
    fn pan_limit(&self, scale: f64) -> (f64, f64) {
        let Some((texture_width, texture_height)) = self.texture_size() else { return (0.0, 0.0) };
        let (view_width, view_height) = self.view_size();
        let width = texture_width * scale;
        let height = texture_height * scale;
        ((width - view_width).max(0.0) / 2.0, (height - view_height).max(0.0) / 2.0)
    }

    /// The view the picture is heading to: its fit or zoom scale, and its pan, kept within the view's edges.
    fn target_view(&self) -> Option<View> {
        let scale = self.current_scale()?;
        let (max_x, max_y) = self.pan_limit(scale);
        let (x, y) = self.imp().offset.get();
        Some(View { scale, offset: (x.clamp(-max_x, max_x), y.clamp(-max_y, max_y)) })
    }

    /// Where a picture drawn as `view` is placed in the view, as x, y, width and height.
    fn placed(&self, view: View) -> Option<(f64, f64, f64, f64)> {
        let (texture_width, texture_height) = self.texture_size()?;
        let (view_width, view_height) = self.view_size();
        let width = texture_width * view.scale;
        let height = texture_height * view.scale;
        let (max_x, max_y) = self.pan_limit(view.scale);
        let x = (view_width - width) / 2.0 + view.offset.0.clamp(-max_x, max_x);
        let y = (view_height - height) / 2.0 + view.offset.1.clamp(-max_y, max_y);
        Some((x, y, width, height))
    }

    /// The filter a picture drawn at `scale` is resampled with.
    fn scaling_filter(&self, scale: f64) -> gsk::ScalingFilter {
        let enlarged = scale > 1.0;
        if self.imp().nearest_scaling.get() || (enlarged && !self.imp().smooth_upscaling.get()) {
            gsk::ScalingFilter::Nearest
        } else {
            gsk::ScalingFilter::Linear
        }
    }

    /// Draws the picture cropped to cover the whole view, blurred, and dimmed, so it fills the background.
    fn draw_underlay(&self, snapshot: &gtk::Snapshot, texture: &gdk::Texture) {
        let Some((texture_width, texture_height)) = self.texture_size() else { return };
        let (view_width, view_height) = self.view_size();
        if view_width <= 0.0 || view_height <= 0.0 {
            return;
        }
        let scale = (view_width / texture_width).max(view_height / texture_height);
        let width = texture_width * scale;
        let height = texture_height * scale;
        let rect = graphene::Rect::new(
            ((view_width - width) / 2.0) as f32,
            ((view_height - height) / 2.0) as f32,
            width as f32,
            height as f32,
        );
        let whole = graphene::Rect::new(0.0, 0.0, view_width as f32, view_height as f32);
        // Clipped to the view: the cropped copy and its blur reach past it, over whatever is beside it.
        snapshot.push_clip(&whole);
        snapshot.push_blur(BLUR_RADIUS);
        snapshot.append_texture(texture, &rect);
        snapshot.pop();
        snapshot.append_color(&DIM, &whole);
        snapshot.pop();
    }
}
