//! A video thumbnail's picture between two film strips: a dark strip down each side with light sprocket
//! holes. It is drawn as the thumbnail is shown, so the picture is not changed and the strips fit it
//! exactly, whatever size it is shown at.

use std::cell::RefCell;

use gtk::gdk::subclass::paintable::PaintableImpl;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

const STRIP_COLOUR: gdk::RGBA = gdk::RGBA::new(0.08, 0.08, 0.08, 1.0);
const HOLE_COLOUR: gdk::RGBA = gdk::RGBA::new(0.85, 0.85, 0.85, 1.0);

/// The width of one strip for a picture `height` pixels high: an eighth of the height, at least 4 pixels.
fn strip_width(height: i32) -> i32 {
    (height / 8).max(4)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FilmStrip {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FilmStrip {
        const NAME: &'static str = "RsimgvFilmStrip";
        type Type = super::FilmStrip;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for FilmStrip {}

    impl PaintableImpl for FilmStrip {
        fn intrinsic_width(&self) -> i32 {
            let texture = self.texture.borrow();
            texture.as_ref().map_or(0, |texture| {
                let height = texture.intrinsic_height();
                texture.intrinsic_width() + 2 * strip_width(height)
            })
        }

        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |texture| texture.intrinsic_height())
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(texture) = self.texture.borrow().clone() else { return };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let (width, height) = (width as f32, height as f32);

            // The strips are in the same proportion to the picture as in its intrinsic size.
            let scale = width / self.obj().intrinsic_width().max(1) as f32;
            let strip = strip_width(texture.intrinsic_height()) as f32 * scale;

            snapshot.append_color(&STRIP_COLOUR, &graphene::Rect::new(0.0, 0.0, width, height));
            snapshot.append_texture(
                &texture,
                &graphene::Rect::new(strip, 0.0, width - 2.0 * strip, height),
            );

            let hole = strip / 2.0;
            let left = (strip - hole) / 2.0;
            let right = width - strip + left;
            let mut y = hole;
            while y + hole <= height {
                for x in [left, right] {
                    snapshot.append_color(&HOLE_COLOUR, &graphene::Rect::new(x, y, hole, hole));
                }
                y += hole * 2.0;
            }
        }
    }
}

glib::wrapper! {
    /// A picture drawn between film strips. Use it wherever a `gdk::Paintable` is shown.
    pub struct FilmStrip(ObjectSubclass<imp::FilmStrip>) @implements gdk::Paintable;
}

impl FilmStrip {
    pub fn new(texture: &gdk::Texture) -> Self {
        let film: Self = glib::Object::new();
        film.imp().texture.replace(Some(texture.clone()));
        film
    }
}
