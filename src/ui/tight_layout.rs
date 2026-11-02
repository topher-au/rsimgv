//! A layout manager that stacks its widget's children in a column with no gap between them. A box leaves a gap of
//! its spacing, and the one inside a preferences page leaves a wide one between its groups.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TightLayout;

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
            for child in children(widget) {
                let (child_minimum, child_natural, ..) = child.measure(orientation, for_size);
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
            let children: Vec<(gtk::Widget, i32, i32)> = children(widget)
                .map(|child| {
                    let (minimum, natural, ..) = child.measure(gtk::Orientation::Vertical, width);
                    (child, minimum, natural)
                })
                .collect();
            // Every child gets at least its minimum height, then what is left, up to each one's natural height in turn.
            let mut spare = (height - children.iter().map(|(_, minimum, _)| minimum).sum::<i32>()).max(0);
            let mut top = 0;
            for (child, minimum, natural) in children {
                let extra = spare.min(natural - minimum);
                spare -= extra;
                child.size_allocate(&gdk::Rectangle::new(0, top, width, minimum + extra), -1);
                top += minimum + extra;
            }
        }
    }

    /// The children of `widget` that take part in the layout, top to bottom.
    fn children(widget: &gtk::Widget) -> impl Iterator<Item = gtk::Widget> {
        std::iter::successors(widget.first_child(), |child| child.next_sibling()).filter(|child| child.should_layout())
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

        tall[1].set_visible(false);
        assert_eq!(column.measure(gtk::Orientation::Vertical, 200).1, 60, "a hidden child takes no room");
        column.allocate(200, 60, -1, None);
        assert_eq!(top_and_height(&tall[2]).0, 20);
    }
}
