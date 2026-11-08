//! A video preview drawn by an embedded player. Its frames are painted where a video's still would be: filling the item
//! when thumbnails are cropped to a square, or fitted between film strips when they are not.

use std::cell::{Cell, RefCell};

use gtk::gdk::subclass::paintable::PaintableImpl;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use super::film_strip::paint_strips;
use crate::mpv::framing;
use crate::mpv::MpvStream;

/// The dark background a video is drawn over, and the width of one film strip.
const BACKGROUND: gdk::RGBA = gdk::RGBA::new(0.08, 0.08, 0.08, 1.0);
const STRIP_WIDTH: f32 = 6.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct VideoPreview {
        pub(super) stream: RefCell<Option<MpvStream>>,
        /// Whether the picture fills the item, cropped at the sides, rather than being fitted between film strips.
        pub(super) square: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VideoPreview {
        const NAME: &'static str = "RsimgvVideoPreview";
        type Type = super::VideoPreview;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for VideoPreview {}

    impl PaintableImpl for VideoPreview {
        fn intrinsic_width(&self) -> i32 {
            self.stream.borrow().as_ref().map_or(0, |stream| stream.intrinsic_width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.stream.borrow().as_ref().map_or(0, |stream| stream.intrinsic_height())
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(stream) = self.stream.borrow().clone() else { return };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let (width, height) = (width as f32, height as f32);
            let whole = graphene::Rect::new(0.0, 0.0, width, height);
            snapshot.append_color(&BACKGROUND, &whole);
            let (stream_width, stream_height) = (f64::from(stream.intrinsic_width()), f64::from(stream.intrinsic_height()));
            if stream_width > 0.0 && stream_height > 0.0 {
                let (x, y, drawn_width, drawn_height) = if self.square.get() {
                    framing::covering(f64::from(width), f64::from(height), stream_width, stream_height, 1.0)
                } else {
                    let strip = f64::from(STRIP_WIDTH.min(width / 4.0));
                    let (x, y, drawn_width, drawn_height) =
                        framing::fitted((f64::from(width) - 2.0 * strip).max(0.0), f64::from(height), stream_width, stream_height);
                    (strip + x, y, drawn_width, drawn_height)
                };
                snapshot.push_clip(&whole);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x as f32, y as f32));
                stream.upcast_ref::<gdk::Paintable>().snapshot(snapshot, drawn_width, drawn_height);
                snapshot.restore();
                snapshot.pop();
            }
            paint_strips(snapshot, width, height);
        }
    }
}

glib::wrapper! {
    /// An embedded player's frames, painted where a video's thumbnail would be.
    pub struct VideoPreview(ObjectSubclass<imp::VideoPreview>) @implements gdk::Paintable;
}

impl VideoPreview {
    /// The frames of `stream`, filling the item when `square` is set. Frames and size changes of the stream are passed on,
    /// so the picture showing this is drawn again as they come.
    pub fn new(stream: &MpvStream, square: bool) -> Self {
        let preview: Self = glib::Object::new();
        preview.imp().stream.replace(Some(stream.clone()));
        preview.imp().square.set(square);
        let weak = preview.downgrade();
        stream.connect_invalidate_contents(move |_| {
            if let Some(preview) = weak.upgrade() {
                preview.invalidate_contents();
            }
        });
        let weak = preview.downgrade();
        stream.connect_invalidate_size(move |_| {
            if let Some(preview) = weak.upgrade() {
                preview.invalidate_size();
            }
        });
        preview
    }
}
