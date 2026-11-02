//! How a browse item's thumbnail is painted: a picture cropped to a square that fills the item, a picture between
//! film strips, or a folder's icon with pictures scattered over it. The thumbnails themselves are not changed, so
//! the same texture serves each way of painting it.

use std::cell::{Cell, RefCell};

use gtk::gdk::subclass::paintable::PaintableImpl;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use super::film_strip::FilmStrip;

/// The side of each scattered picture, as a share of the shorter side of the item, when up to `PHOTO_FULL_COUNT`
/// are shown. More pictures than that are smaller, so they fit.
pub const PHOTO_SHARE: f32 = 0.3;
const PHOTO_FULL_COUNT: f32 = 4.0;
/// The most pictures a scattered folder shows at once.
pub const MAX_PHOTOS: usize = 12;
/// The white border round each scattered picture, as a share of its longest side.
const PHOTO_BORDER: f32 = 0.025;
const PHOTO_FRAME: gdk::RGBA = gdk::RGBA::new(0.97, 0.97, 0.97, 1.0);
/// Where the scattered pictures are centred, as shares of the item's width and height, in the order they are laid.
/// The most pictures shown at once is the length of this list.
const PHOTO_SPOTS: [(f32, f32); MAX_PHOTOS] = [
    (0.3, 0.36),
    (0.7, 0.32),
    (0.32, 0.7),
    (0.7, 0.68),
    (0.5, 0.5),
    (0.2, 0.52),
    (0.8, 0.5),
    (0.5, 0.2),
    (0.5, 0.82),
    (0.18, 0.18),
    (0.82, 0.84),
    (0.82, 0.18),
];

/// Paints `texture` over the square at (`x`, `y`) of `width` by `height`: scaled to cover it, its overflow cut off.
fn paint_cover(snapshot: &gtk::Snapshot, texture: &gdk::Texture, x: f32, y: f32, width: f32, height: f32) {
    let (texture_width, texture_height) = (texture.intrinsic_width() as f32, texture.intrinsic_height() as f32);
    if texture_width <= 0.0 || texture_height <= 0.0 {
        return;
    }
    let scale = (width / texture_width).max(height / texture_height);
    let (drawn_width, drawn_height) = (texture_width * scale, texture_height * scale);
    snapshot.push_clip(&graphene::Rect::new(x, y, width, height));
    snapshot.append_texture(
        texture,
        &graphene::Rect::new(x + (width - drawn_width) / 2.0, y + (height - drawn_height) / 2.0, drawn_width, drawn_height),
    );
    snapshot.pop();
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SquareThumbnail {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SquareThumbnail {
        const NAME: &'static str = "RsimgvSquareThumbnail";
        type Type = super::SquareThumbnail;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for SquareThumbnail {}

    impl PaintableImpl for SquareThumbnail {
        fn intrinsic_width(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |texture| texture.intrinsic_width().min(texture.intrinsic_height()))
        }

        fn intrinsic_height(&self) -> i32 {
            self.intrinsic_width()
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(texture) = self.texture.borrow().clone() else { return };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            paint_cover(snapshot, &texture, 0.0, 0.0, width as f32, height as f32);
        }
    }

    #[derive(Default)]
    pub struct ScatteredThumbnail {
        /// The folder's icon, which fills the item behind the pictures.
        pub(super) base: RefCell<Option<gdk::Paintable>>,
        /// One picture per slot, in the order they are laid, with how far it is turned in degrees. A slot with no
        /// picture is empty.
        pub(super) photos: RefCell<Vec<Option<(gdk::Texture, f32)>>>,
        /// Whether each picture is drawn at its own proportions, as many as fit, rather than shared out evenly.
        pub(super) fitted: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ScatteredThumbnail {
        const NAME: &'static str = "RsimgvScatteredThumbnail";
        type Type = super::ScatteredThumbnail;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for ScatteredThumbnail {}

    impl PaintableImpl for ScatteredThumbnail {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let base = self.base.borrow().clone();
            let photos = self.photos.borrow().clone();
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            if let Some(base) = base {
                base.snapshot(snapshot, width, height);
            }
            let (width, height) = (width as f32, height as f32);
            let shown = photos.iter().flatten().count();
            let share = if self.fitted.get() {
                PHOTO_SHARE
            } else {
                PHOTO_SHARE * (PHOTO_FULL_COUNT / photos.len().max(PHOTO_FULL_COUNT as usize) as f32).sqrt()
            };
            let longest = width.min(height) * share;
            let border = longest * PHOTO_BORDER;
            for (index, photo) in photos.iter().enumerate() {
                let Some((texture, turn)) = photo else { continue };
                let (texture_width, texture_height) = (texture.intrinsic_width() as f32, texture.intrinsic_height() as f32);
                if texture_width <= 0.0 || texture_height <= 0.0 {
                    continue;
                }
                // The picture keeps its proportions, with its longest side as long as a photo's side is.
                let scale = longest / texture_width.max(texture_height);
                let (drawn_width, drawn_height) = (texture_width * scale, texture_height * scale);
                let (cx, cy) = if shown == 1 { (0.5, 0.5) } else { PHOTO_SPOTS[index] };
                snapshot.save();
                snapshot.translate(&graphene::Point::new(width * cx, height * cy));
                snapshot.rotate(*turn);
                snapshot.append_color(
                    &PHOTO_FRAME,
                    &graphene::Rect::new(
                        -drawn_width / 2.0 - border,
                        -drawn_height / 2.0 - border,
                        drawn_width + 2.0 * border,
                        drawn_height + 2.0 * border,
                    ),
                );
                snapshot.append_texture(
                    texture,
                    &graphene::Rect::new(-drawn_width / 2.0, -drawn_height / 2.0, drawn_width, drawn_height),
                );
                snapshot.restore();
            }
        }
    }
}

glib::wrapper! {
    /// A picture cropped to a square that fills the item it is shown in.
    pub struct SquareThumbnail(ObjectSubclass<imp::SquareThumbnail>) @implements gdk::Paintable;
}

impl SquareThumbnail {
    pub fn new(texture: &gdk::Texture) -> Self {
        let square: Self = glib::Object::new();
        square.imp().texture.replace(Some(texture.clone()));
        square
    }
}

glib::wrapper! {
    /// A folder's icon with pictures scattered over it, each turned a little, the way photographs lie on a table.
    pub struct ScatteredThumbnail(ObjectSubclass<imp::ScatteredThumbnail>) @implements gdk::Paintable;
}

impl ScatteredThumbnail {
    /// A folder showing `base`, with room for `slots` pictures, none of them made yet. `slots` is at most the
    /// number of places a picture can go.
    pub fn new(base: Option<gdk::Paintable>, slots: usize, fitted: bool) -> Self {
        let scattered: Self = glib::Object::new();
        scattered.imp().base.replace(base);
        scattered.imp().photos.replace(vec![None; slots.min(MAX_PHOTOS)]);
        scattered.imp().fitted.set(fitted);
        scattered
    }

    /// Shows `page` in place of the pictures shown: one entry for each slot, empty where there is no picture.
    pub fn set_page(&self, page: Vec<Option<(gdk::Texture, f32)>>) {
        self.imp().photos.replace(page);
        self.invalidate_contents();
    }

    /// Shows the picture `texture`, turned `turn` degrees, as photo `index`.
    pub fn set_photo(&self, index: usize, texture: &gdk::Texture, turn: f32) {
        if let Some(photo) = self.imp().photos.borrow_mut().get_mut(index) {
            *photo = Some((texture.clone(), turn));
        }
        self.invalidate_contents();
    }

    /// How many pictures are up.
    #[cfg(test)]
    pub fn shown(&self) -> usize {
        self.imp().photos.borrow().iter().flatten().count()
    }

    /// Takes photo `index` away, so its place is empty.
    pub fn clear_photo(&self, index: usize) {
        if let Some(photo) = self.imp().photos.borrow_mut().get_mut(index) {
            if photo.take().is_some() {
                self.invalidate_contents();
            }
        }
    }
}

/// The paintable that shows a picture: cropped to a square if `crop`, between film strips if `film`, or as it is.
pub fn picture_paintable(texture: &gdk::Texture, crop: bool, film: bool) -> gdk::Paintable {
    if crop {
        SquareThumbnail::new(texture).upcast()
    } else if film {
        FilmStrip::new(texture).upcast()
    } else {
        texture.clone().upcast()
    }
}
