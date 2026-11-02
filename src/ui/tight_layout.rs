//! A layout manager that stacks its widget's children in a column with no gap between them. A box leaves a gap of
//! its spacing, and the one inside a preferences page leaves a wide one between its groups. The first child can be
//! indented, even while it is hidden, which sets a group's title in from its rows and leaves them where they are when
//! the group has no title.

use std::cell::Cell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TightLayout {
        /// How far in from the left the first child starts, in pixels.
        pub(super) first_indent: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TightLayout {
        const NAME: &'static str = "RsimgvTightLayout";
        type Type = super::TightLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for TightLayout {}

    impl LayoutManagerImpl for TightLayout {
        fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (mut minimum, mut natural) = (0, 0);
            for (index, child) in children(widget) {
                let indent = self.indent(index);
                let (child_minimum, child_natural, ..) = if orientation == gtk::Orientation::Vertical {
                    child.measure(orientation, if for_size < 0 { for_size } else { (for_size - indent).max(0) })
                } else {
                    let (minimum, natural, ..) = child.measure(orientation, for_size);
                    (minimum + indent, natural + indent, 0, 0)
                };
                if orientation == gtk::Orientation::Vertical {
                    minimum += child_minimum;
                    natural += child_natural;
                } else {
                    minimum = minimum.max(child_minimum);
                    natural = natural.max(child_natural);
                }
            }
            (minimum, natural, -1, -1)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, _baseline: i32) {
            let children: Vec<(gtk::Widget, i32, i32, i32)> = children(widget)
                .map(|(index, child)| {
                    let indent = self.indent(index);
                    let (minimum, natural, ..) = child.measure(gtk::Orientation::Vertical, (width - indent).max(0));
                    (child, indent, minimum, natural)
                })
                .collect();
            // Every child gets at least its minimum height, then what is left, up to each one's natural height in turn.
            let mut spare = (height - children.iter().map(|(_, _, minimum, _)| minimum).sum::<i32>()).max(0);
            let mut top = 0;
            for (child, indent, minimum, natural) in children {
                let extra = spare.min(natural - minimum);
                spare -= extra;
                child.size_allocate(&gdk::Rectangle::new(indent, top, (width - indent).max(0), minimum + extra), -1);
                top += minimum + extra;
            }
        }
    }

    impl TightLayout {
        /// How far in from the left the child at `index` starts.
        fn indent(&self, index: usize) -> i32 {
            if index == 0 { self.first_indent.get() } else { 0 }
        }
    }

    /// The children of `widget` that take part in the layout, top to bottom, each with its place among all the
    /// children, hidden ones too.
    fn children(widget: &gtk::Widget) -> impl Iterator<Item = (usize, gtk::Widget)> {
        std::iter::successors(widget.first_child(), |child| child.next_sibling())
            .enumerate()
            .filter(|(_, child)| child.should_layout())
    }
}

glib::wrapper! {
    /// Stacks the children in a column with no gap between them.
    pub struct TightLayout(ObjectSubclass<imp::TightLayout>) @extends gtk::LayoutManager;
}

impl Default for TightLayout {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TightLayout {
    /// A layout whose first child starts `indent` pixels in from the left.
    pub fn with_first_indent(indent: i32) -> Self {
        let layout = Self::default();
        layout.imp().first_indent.set(indent);
        layout
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run_scenarios() {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 32);
        column.set_layout_manager(Some(TightLayout::default()));
        let tall = [20, 30, 40].map(|height| {
            let child = gtk::Label::new(None);
            child.set_height_request(height);
            column.append(&child);
            child
        });

        let (minimum, natural, ..) = column.measure(gtk::Orientation::Vertical, 200);
        assert_eq!((minimum, natural), (90, 90), "the heights add up, with no gap");
        column.allocate(200, 90, -1, None);
        let top_and_height = |child: &gtk::Label| {
            let bounds = child.compute_bounds(&column).expect("the child is in the column");
            (bounds.y() as i32, bounds.height() as i32)
        };
        let tops = tall.each_ref().map(top_and_height);
        assert_eq!(tops, [(0, 20), (20, 30), (50, 40)], "each child sits directly under the last");

        // The first child can start further in, and takes the width that leaves.
        column.set_layout_manager(Some(TightLayout::with_first_indent(10)));
        tall[1].set_visible(true);
        let (minimum_width, ..) = column.measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum_width >= 10, "the indent is part of the width wanted");
        column.allocate(200, 90, -1, None);
        let bounds = tall[0].compute_bounds(&column).expect("the child is in the column");
        assert_eq!((bounds.x() as i32, bounds.width() as i32), (10, 190), "the first child is set in");
        let bounds = tall[1].compute_bounds(&column).expect("the child is in the column");
        assert_eq!((bounds.x() as i32, bounds.width() as i32), (0, 200), "the others are not");

        // The indent belongs to the first child, so while that is hidden nothing else takes it.
        tall[0].set_visible(false);
        column.allocate(200, 70, -1, None);
        let bounds = tall[1].compute_bounds(&column).expect("the child is in the column");
        assert_eq!((bounds.x() as i32, bounds.width() as i32), (0, 200), "the rows of an untitled group are not set in");
        tall[0].set_visible(true);

        tall[1].set_visible(false);
        assert_eq!(column.measure(gtk::Orientation::Vertical, 200).1, 60, "a hidden child takes no room");
        column.allocate(200, 60, -1, None);
        assert_eq!(top_and_height(&tall[2]).0, 20);
    }
}
