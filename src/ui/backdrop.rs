//! A picture drawn to cover the whole widget, heavily blurred and dimmed, to fill the background behind a video until
//! its frames are up, or round them where they do not reach.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

/// The radius of the blur, in pixels.
const BLUR_RADIUS: f64 = 60.0;
/// How dark the picture is made: the black laid over it.
const DIM: gdk::RGBA = gdk::RGBA::new(0.0, 0.0, 0.0, 0.55);

/// The rectangle that covers a `view_width` by `view_height` view with a `width` by `height` picture, centred, as
/// (x, y, width, height).
fn cover(view_width: f64, view_height: f64, width: f64, height: f64) -> (f64, f64, f64, f64) {
    let scale = (view_width / width).max(view_height / height);
    let (covered_width, covered_height) = (width * scale, height * scale);
    ((view_width - covered_width) / 2.0, (view_height - covered_height) / 2.0, covered_width, covered_height)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Backdrop {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Backdrop {
        const NAME: &'static str = "RsimgvBackdrop";
        type Type = super::Backdrop;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Backdrop {}

    impl WidgetImpl for Backdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let Some(texture) = self.texture.borrow().clone() else { return };
            let (view_width, view_height) = (f64::from(widget.width()), f64::from(widget.height()));
            if view_width <= 0.0 || view_height <= 0.0 || texture.width() <= 0 || texture.height() <= 0 {
                return;
            }
            let (x, y, width, height) = cover(view_width, view_height, f64::from(texture.width()), f64::from(texture.height()));
            let whole = graphene::Rect::new(0.0, 0.0, view_width as f32, view_height as f32);
            // Clipped to the widget: the covering copy and its blur reach past it.
            snapshot.push_clip(&whole);
            snapshot.push_blur(BLUR_RADIUS);
            snapshot.append_texture(&texture, &graphene::Rect::new(x as f32, y as f32, width as f32, height as f32));
            snapshot.pop();
            snapshot.append_color(&DIM, &whole);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// A blurred, dimmed picture that covers the widget.
    pub struct Backdrop(ObjectSubclass<imp::Backdrop>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Backdrop {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Backdrop {
    /// Shows `texture` behind everything, or nothing for `None`.
    pub fn set_texture(&self, texture: Option<&gdk::Texture>) {
        self.imp().texture.replace(texture.cloned());
        self.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::cover;

    #[test]
    fn a_picture_covers_the_view_and_is_centred() {
        // A wide picture in a tall view is as tall as the view, and cropped at the sides.
        let (x, y, width, height) = cover(400.0, 800.0, 200.0, 100.0);
        assert_eq!((y, height), (0.0, 800.0));
        assert!((width - 1600.0).abs() < 1e-9 && (x + 600.0).abs() < 1e-9);
        // A tall picture in a wide view is as wide as the view.
        let (x, y, width, height) = cover(800.0, 400.0, 100.0, 200.0);
        assert_eq!((x, width), (0.0, 800.0));
        assert!((height - 1600.0).abs() < 1e-9 && (y + 600.0).abs() < 1e-9);
    }
}
