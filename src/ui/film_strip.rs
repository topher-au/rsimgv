//! A video thumbnail's picture between two film strips: a dark strip down each side with light sprocket
//! holes. It is drawn as the thumbnail is shown, so the picture is not changed. The strips are the same small
//! size whatever size the thumbnail is shown at.

use std::cell::RefCell;

use gtk::gdk::subclass::paintable::PaintableImpl;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

const STRIP_COLOUR: gdk::RGBA = gdk::RGBA::new(0.08, 0.08, 0.08, 1.0);
const HOLE_COLOUR: gdk::RGBA = gdk::RGBA::new(0.85, 0.85, 0.85, 1.0);

/// The width of one strip, in pixels as drawn.
const STRIP_WIDTH: f32 = 6.0;
/// The size of a sprocket hole, and the distance from one hole to the next down a strip.
const HOLE_SIZE: f32 = 3.0;
const HOLE_PITCH: f32 = 6.0;

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
            texture.as_ref().map_or(0, |texture| texture.intrinsic_width() + 2 * STRIP_WIDTH as i32)
        }

        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |texture| texture.intrinsic_height())
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(texture) = self.texture.borrow().clone() else { return };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let (width, height) = (width as f32, height as f32);

            // The strips are a set size, so the picture is fitted into what they leave, keeping its proportions.
            let strip = STRIP_WIDTH.min(width / 4.0);
            let free = (width - 2.0 * strip).max(0.0);
            let (picture_width, picture_height) = (texture.intrinsic_width().max(1) as f32, texture.intrinsic_height().max(1) as f32);
            let scale = (free / picture_width).min(height / picture_height);
            let (shown_width, shown_height) = (picture_width * scale, picture_height * scale);

            snapshot.append_color(&STRIP_COLOUR, &graphene::Rect::new(0.0, 0.0, width, height));
            snapshot.append_texture(
                &texture,
                &graphene::Rect::new(strip + (free - shown_width) / 2.0, (height - shown_height) / 2.0, shown_width, shown_height),
            );

            let left = (strip - HOLE_SIZE) / 2.0;
            let right = width - strip + left;
            let mut y = (HOLE_PITCH - HOLE_SIZE) / 2.0;
            while y + HOLE_SIZE <= height {
                for x in [left, right] {
                    snapshot.append_color(&HOLE_COLOUR, &graphene::Rect::new(x, y, HOLE_SIZE, HOLE_SIZE));
                }
                y += HOLE_PITCH;
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
