//! A single line of text drawn turned by an angle. The widget's size and layout are those of the turned text: a
//! quarter turn makes it as wide as the text is tall and as tall as the text is wide. The text takes the font and the
//! colour the stylesheet gives the widget, whose CSS name is `rotated-label`.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene};

/// The width and height of the box that holds a `width` by `height` rectangle turned `degrees` clockwise.
fn rotated_extent(width: f64, height: f64, degrees: f64) -> (f64, f64) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (width * cos.abs() + height * sin.abs(), width * sin.abs() + height * cos.abs())
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RotatedLabel {
        pub(super) text: RefCell<String>,
        /// Whether the text is Pango markup.
        pub(super) markup: Cell<bool>,
        /// The clockwise angle the text is turned by, in degrees.
        pub(super) angle: Cell<f64>,
        /// Shadows drawn under the text, top one first: how far across and down, how blurred, and their colour.
        pub(super) shadows: RefCell<Vec<(f32, f32, f32, gtk::gdk::RGBA)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RotatedLabel {
        const NAME: &'static str = "RsimgvRotatedLabel";
        type Type = super::RotatedLabel;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("rotated-label");
        }
    }

    impl ObjectImpl for RotatedLabel {}

    impl WidgetImpl for RotatedLabel {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (width, height) = self.obj().turned_size();
            let size = if orientation == gtk::Orientation::Horizontal { width } else { height };
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let label = self.obj();
            let layout = label.text_layout();
            let (text_width, text_height) = layout.pixel_size();
            // The text is drawn upright about the middle of the widget, which is turned about its own middle.
            snapshot.translate(&graphene::Point::new(label.width() as f32 / 2.0, label.height() as f32 / 2.0));
            // The shadows fall in the same direction whatever the angle, and the top one is drawn last of them.
            for &(dx, dy, blur, color) in self.shadows.borrow().iter().rev() {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(dx, dy));
                snapshot.rotate(self.angle.get() as f32);
                snapshot.translate(&graphene::Point::new(-(text_width as f32) / 2.0, -(text_height as f32) / 2.0));
                if blur > 0.0 {
                    snapshot.push_blur(f64::from(blur) / 2.0);
                    snapshot.append_layout(&layout, &color);
                    snapshot.pop();
                } else {
                    snapshot.append_layout(&layout, &color);
                }
                snapshot.restore();
            }
            snapshot.rotate(self.angle.get() as f32);
            snapshot.translate(&graphene::Point::new(-(text_width as f32) / 2.0, -(text_height as f32) / 2.0));
            snapshot.append_layout(&layout, &label.color());
        }
    }
}

glib::wrapper! {
    /// A line of text turned by an angle.
    pub struct RotatedLabel(ObjectSubclass<imp::RotatedLabel>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RotatedLabel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RotatedLabel {
    pub fn new(text: &str, degrees: f64) -> Self {
        let label = Self::default();
        label.imp().text.replace(text.to_owned());
        label.imp().angle.set(degrees);
        label
    }

    /// Sets the text.
    pub fn set_text(&self, text: &str) {
        if *self.imp().text.borrow() != text || self.imp().markup.replace(false) {
            self.imp().markup.set(false);
            self.imp().text.replace(text.to_owned());
            self.update_property(&[gtk::accessible::Property::Label(text)]);
            self.queue_resize();
        }
    }

    /// Sets the text to `markup`, which Pango markup the layout renders. Markup that is not valid is the text as it is.
    pub fn set_markup(&self, markup: &str) {
        if gtk::pango::parse_markup(markup, '\0').is_err() {
            self.set_text(markup);
            return;
        }
        if *self.imp().text.borrow() != markup || !self.imp().markup.replace(true) {
            self.imp().markup.set(true);
            self.imp().text.replace(markup.to_owned());
            self.queue_resize();
        }
    }

    pub fn text(&self) -> String {
        self.imp().text.borrow().clone()
    }

    /// Turns the text `degrees` clockwise. The widget's size changes to hold it.
    pub fn set_angle(&self, degrees: f64) {
        if (self.imp().angle.replace(degrees) - degrees).abs() > f64::EPSILON {
            self.queue_resize();
        }
    }

    /// Draws `shadows` under the text, top one first: how far across and down each falls, how blurred it is, and its colour.
    pub fn set_shadows(&self, shadows: Vec<(f32, f32, f32, gtk::gdk::RGBA)>) {
        self.imp().shadows.replace(shadows);
        self.queue_draw();
    }

    pub fn angle(&self) -> f64 {
        self.imp().angle.get()
    }

    fn text_layout(&self) -> gtk::pango::Layout {
        let layout = self.create_pango_layout(None);
        if self.imp().markup.get() {
            layout.set_markup(&self.imp().text.borrow());
        } else {
            layout.set_text(&self.imp().text.borrow());
        }
        layout
    }

    /// The size of the box that holds the turned text, in whole pixels.
    fn turned_size(&self) -> (i32, i32) {
        let (width, height) = self.text_layout().pixel_size();
        let (width, height) = rotated_extent(f64::from(width), f64::from(height), self.imp().angle.get());
        (width.ceil() as i32, height.ceil() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::rotated_extent;

    #[test]
    fn a_quarter_turn_swaps_the_width_and_the_height() {
        for degrees in [90.0, 270.0, -90.0] {
            let (width, height) = rotated_extent(60.0, 20.0, degrees);
            assert!((width - 20.0).abs() < 1e-9 && (height - 60.0).abs() < 1e-9, "{degrees}: {width} by {height}");
        }
    }

    #[test]
    fn no_turn_and_a_half_turn_keep_the_size() {
        for degrees in [0.0, 180.0] {
            let (width, height) = rotated_extent(60.0, 20.0, degrees);
            assert!((width - 60.0).abs() < 1e-9 && (height - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_slant_holds_the_whole_text() {
        // At 45 degrees a 60 by 20 line spans (60 + 20) / sqrt(2) each way.
        let (width, height) = rotated_extent(60.0, 20.0, 45.0);
        let expected = 80.0 / 2f64.sqrt();
        assert!((width - expected).abs() < 1e-9 && (height - expected).abs() < 1e-9);
    }
}
