//! The picture view for images. The picture is drawn at the size its fit mode gives, or at a zoom the user chose,
//! centred in the view and panned by an offset. Zooming and panning ease to their new place over a few frames. A
//! blurred, dimmed copy of the picture can fill the view behind it.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

/// How much one zoom step changes the scale.
const ZOOM_STEP: f64 = 1.25;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Window,
    Width,
    Height,
    Original,
}

impl Fit {
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
        /// The scale used while no fit mode is set.
        pub(super) zoom: Cell<f64>,
        /// The offset the picture is panned to: where it is heading.
        pub(super) offset: Cell<(f64, f64)>,
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
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImageCanvas {
        const NAME: &'static str = "RsimgvImageCanvas";
        type Type = super::ImageCanvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ImageCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.fit.set(Some(Fit::Window));
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
            let view = match self.shown.get() {
                Some(view) => view,
                None => {
                    self.shown.set(Some(target));
                    target
                }
            };
            // A change in the view's size changes the target, so the picture eases to it.
            if view != target {
                canvas.ease();
            }
            let Some((x, y, width, height)) = canvas.placed(view) else { return };
            snapshot.append_texture(&texture, &graphene::Rect::new(x as f32, y as f32, width as f32, height as f32));
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
    /// Shows `texture`, or nothing. The picture is centred again, without easing from the last one.
    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        self.imp().texture.replace(texture);
        self.imp().offset.set((0.0, 0.0));
        self.snap();
    }

    /// Fits the picture to the view in `fit` mode, centred again.
    pub fn set_fit(&self, fit: Fit) {
        self.imp().fit.set(Some(fit));
        self.imp().offset.set((0.0, 0.0));
        self.ease();
    }

    /// Moves to the fit mode after the current one. From a zoom, that is the first fit mode.
    pub fn cycle_fit(&self) {
        let next = self.imp().fit.get().map_or(Fit::Window, Fit::next);
        self.set_fit(next);
    }

    pub fn zoom_in(&self) {
        self.zoom_by(ZOOM_STEP);
    }

    pub fn zoom_out(&self) {
        self.zoom_by(1.0 / ZOOM_STEP);
    }

    /// Pans the picture by `dx` and `dy` pixels, easing there, as far as it can go while still covering the view.
    pub fn pan(&self, dx: f64, dy: f64) {
        let (x, y) = self.imp().offset.get();
        self.imp().offset.set((x + dx, y + dy));
        self.ease();
    }

    /// Draws a blurred, dimmed copy of the picture, cropped to fill the view, behind the picture. Off by default.
    pub fn set_underlay(&self, on: bool) {
        self.imp().underlay.set(on);
        self.queue_draw();
    }

    fn zoom_by(&self, factor: f64) {
        let Some(scale) = self.current_scale() else { return };
        self.imp().zoom.set((scale * factor).clamp(MIN_SCALE, MAX_SCALE));
        self.imp().fit.set(None);
        self.ease();
    }

    /// Starts easing the shown view toward the target, if it is not already doing so.
    fn ease(&self) {
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
        Some(match self.imp().fit.get() {
            Some(Fit::Window) => (view_width / texture_width).min(view_height / texture_height),
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
