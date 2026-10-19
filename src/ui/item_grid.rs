//! The grid of browse items. It is a plain widget with its own layout: each item keeps its width,
//! rows are filled left to right and centred, every item in a row is as tall as the tallest one there,
//! and only the items on screen have widgets.
//!
//! It sits in a `GtkScrolledWindow`, which does the scrolling. The grid is as tall as all its rows,
//! so the scroller offsets it; the grid reads the scroller's adjustment to know what is visible.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::fs::FileEntry;

/// Space between items horizontally and vertically, in pixels.
const SPACING_X: i32 = 8;
const SPACING_Y: i32 = 8;

/// Builds an item's widget once, and fills it in for an item each time it is shown.
pub struct Builders {
    setup: Box<dyn Fn() -> gtk::Widget>,
    bind: Box<dyn Fn(&gtk::Widget, &FileEntry, u32)>,
}

/// How many items fit across for a width.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Columns {
    cell_w: i32,
    count: i32,
}

impl Columns {
    fn new(cell_w: i32, width: i32) -> Self {
        let cell_w = cell_w.max(1);
        Self {
            cell_w,
            count: ((width + SPACING_X) / (cell_w + SPACING_X)).max(1),
        }
    }

    fn rows(self, items: u32) -> usize {
        (items as usize).div_ceil(self.count as usize)
    }
}

/// The height of each row and where each row starts. A row is as tall as its tallest item, once it has
/// been measured; rows not yet measured use the default item height.
struct RowTable {
    /// Where each row starts, with one extra entry for the end of the last row.
    tops: Vec<i32>,
    heights: Vec<i32>,
}

impl RowTable {
    fn new(columns: Columns, items: u32, heights: &HashMap<usize, i32>, default: i32) -> Self {
        let rows = columns.rows(items);
        let heights: Vec<i32> = (0..rows).map(|row| heights.get(&row).copied().unwrap_or(default)).collect();
        let mut tops = Vec::with_capacity(rows + 1);
        let mut top = 0;
        for height in &heights {
            tops.push(top);
            top += height + SPACING_Y;
        }
        tops.push(top);
        Self { tops, heights }
    }

    /// The height of the grid: all rows, with the spacing between them.
    fn total(&self) -> i32 {
        match self.heights.len() {
            0 => 0,
            _ => self.tops[self.heights.len()] - SPACING_Y,
        }
    }

    /// The row that contains the height `y`, clamped to the grid.
    fn row_at(&self, y: f64) -> usize {
        let rows = self.heights.len();
        let after = self.tops[..rows].partition_point(|top| f64::from(*top) <= y);
        after.saturating_sub(1).min(rows.saturating_sub(1))
    }

    /// The top-left corner of the item at `index` in a grid `width` wide. Each row is centred.
    fn origin(&self, columns: Columns, index: u32, items: u32, width: i32) -> (i32, i32) {
        let row = index as usize / columns.count as usize;
        let col = index as i32 % columns.count;
        let in_row = (items as i32 - (row as i32) * columns.count).min(columns.count);
        let row_width = in_row * columns.cell_w + (in_row - 1) * SPACING_X;
        let x0 = ((width - row_width) / 2).max(0);
        (x0 + col * (columns.cell_w + SPACING_X), self.tops[row])
    }
}

mod layout {
    use super::*;

    /// Lays out an `ItemGrid`: measures it as its rows, and places each child in its row.
    #[derive(Default)]
    pub struct ItemLayout;

    #[glib::object_subclass]
    impl ObjectSubclass for ItemLayout {
        const NAME: &'static str = "RsimgvItemLayout";
        type Type = super::ItemLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for ItemLayout {}

    impl LayoutManagerImpl for ItemLayout {
        fn measure(&self, widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(grid) = widget.downcast_ref::<super::ItemGrid>() else {
                return (0, 0, -1, -1);
            };
            match orientation {
                gtk::Orientation::Horizontal => {
                    let width = grid.cell_size().0;
                    (width, width, -1, -1)
                }
                _ => {
                    // GTK asks for the height before the width is known, when `for_size` is -1. The grid is
                    // then measured at the width it has, so the scroller gets the height of its real rows.
                    let width = if for_size > 0 { for_size } else { widget.width().max(grid.cell_size().0) };
                    let height = grid.table(width).total();
                    (height, height, -1, -1)
                }
            }
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, _height: i32, _baseline: i32) {
            let Some(grid) = widget.downcast_ref::<super::ItemGrid>() else { return };
            // The items on screen are settled first, so the ones shown are the ones placed.
            grid.update_shown(width);
            let columns = grid.columns(width);
            let items = grid.item_count();
            let table = grid.table(width);
            let mut child = widget.first_child();
            while let Some(cell) = child {
                child = cell.next_sibling();
                let Ok(index) = cell.widget_name().parse::<u32>() else { continue };
                let row = index as usize / columns.count as usize;
                let (x, y) = table.origin(columns, index, items, width);
                let transform = gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32));
                cell.allocate(columns.cell_w, table.heights[row], -1, Some(transform));
            }
        }
    }
}

glib::wrapper! {
    /// The layout that places an `ItemGrid`'s items.
    pub struct ItemLayout(ObjectSubclass<layout::ItemLayout>) @extends gtk::LayoutManager;
}

impl Default for ItemLayout {
    fn default() -> Self {
        glib::Object::new()
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ItemGrid {
        pub(super) model: RefCell<Option<gtk::SingleSelection>>,
        pub(super) builders: RefCell<Option<Rc<Builders>>>,
        /// The widgets on screen, by item position.
        pub(super) shown: RefCell<HashMap<u32, gtk::Widget>>,
        /// Widgets no longer shown, kept to be reused.
        pub(super) spare: RefCell<Vec<gtk::Widget>>,
        /// The item width and the default item height, in pixels.
        pub(super) cell_size: Cell<(i32, i32)>,
        /// The measured height of each row, by row number, for the columns in `measured_columns`.
        pub(super) row_heights: RefCell<HashMap<usize, i32>>,
        pub(super) measured_columns: Cell<i32>,
        /// The scroller's adjustment this grid listens to, and the handler on it.
        pub(super) adjustment: RefCell<Option<(glib::WeakRef<gtk::Adjustment>, glib::SignalHandlerId)>>,
        /// The selection whose changes are followed, so they are connected once.
        pub(super) connected: RefCell<Option<glib::WeakRef<gtk::SingleSelection>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemGrid {
        const NAME: &'static str = "RsimgvItemGrid";
        type Type = super::ItemGrid;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<super::ItemLayout>();
            klass.set_css_name("rsimgv-item-grid");
        }
    }

    impl ObjectImpl for ItemGrid {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_focusable(true);
            self.cell_size.set((128, 128));

            let click = gtk::GestureClick::new();
            click.connect_pressed({
                let grid = obj.downgrade();
                move |gesture, n_press, x, y| {
                    let Some(grid) = grid.upgrade() else { return };
                    grid.grab_focus();
                    if gesture.current_button() != gdk::BUTTON_PRIMARY {
                        return;
                    }
                    if let Some(position) = grid.position_at(x, y) {
                        grid.select(position);
                        if n_press == 2 {
                            grid.emit_by_name::<()>("activate", &[&position]);
                        }
                    }
                }
            });
            obj.add_controller(click);

            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed({
                let grid = obj.downgrade();
                move |_, key, _, _| {
                    let Some(grid) = grid.upgrade() else { return glib::Propagation::Proceed };
                    grid.handle_key(key)
                }
            });
            obj.add_controller(keys);
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> = LazyLock::new(|| {
                vec![glib::subclass::Signal::builder("activate")
                    .param_types([u32::static_type()])
                    .build()]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for ItemGrid {}
}

glib::wrapper! {
    /// A scrollable-by-its-parent grid of items, laid out as centred rows of equal-height items.
    pub struct ItemGrid(ObjectSubclass<imp::ItemGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ItemGrid {
    /// Sets how each item is built and filled in. `setup` makes a widget for one item; `bind` fills it in
    /// for the item at a position.
    pub fn set_builders(
        &self,
        setup: impl Fn() -> gtk::Widget + 'static,
        bind: impl Fn(&gtk::Widget, &FileEntry, u32) + 'static,
    ) {
        self.imp().builders.replace(Some(Rc::new(Builders {
            setup: Box::new(setup),
            bind: Box::new(bind),
        })));
    }

    /// The width of each item and its default height, in pixels. Rows that have not been measured use
    /// the default height; a row is as tall as its tallest item once it is shown.
    pub fn set_cell_size(&self, width: i32, height: i32) {
        if self.imp().cell_size.get() != (width, height) {
            self.imp().cell_size.set((width, height));
            self.refresh();
        }
    }

    pub fn cell_size(&self) -> (i32, i32) {
        self.imp().cell_size.get()
    }

    pub fn model(&self) -> Option<gtk::SingleSelection> {
        self.imp().model.borrow().clone()
    }

    /// Shows the items of `selection`, or none. The selection's changes are followed until it is replaced.
    pub fn set_model(&self, selection: Option<&gtk::SingleSelection>) {
        self.reset();
        self.imp().model.take();
        if let Some(selection) = selection {
            let already = self.imp().connected.borrow().as_ref().and_then(|weak| weak.upgrade());
            if already.as_ref() != Some(selection) {
                self.imp().connected.replace(Some(selection.downgrade()));
                self.follow_selection(selection);
            }
            self.imp().model.replace(Some(selection.clone()));
        }
        self.queue_resize();
    }

    /// Follows the changes to `selection`: its items changing relays the grid out, and its selection
    /// changing redraws which item looks selected.
    fn follow_selection(&self, selection: &gtk::SingleSelection) {
        selection.connect_items_changed({
            let grid = self.downgrade();
            move |_, _, _, _| {
                if let Some(grid) = grid.upgrade() {
                    grid.reset();
                    grid.queue_resize();
                }
            }
        });
        selection.connect_selection_changed({
            let grid = self.downgrade();
            move |_, _, _| {
                if let Some(grid) = grid.upgrade() {
                    grid.update_selected_classes();
                }
            }
        });
    }

    pub fn item_count(&self) -> u32 {
        self.imp().model.borrow().as_ref().map_or(0, |model| model.n_items())
    }

    /// Rebinds every shown item and lays the grid out again, after its items or their look changed.
    pub fn refresh(&self) {
        self.reset();
        self.queue_resize();
    }

    /// Removes the shown items and forgets the row heights measured for them.
    fn reset(&self) {
        self.clear_children();
        self.imp().row_heights.borrow_mut().clear();
    }

    /// Selects the item at `position`, and scrolls it into view.
    pub fn select(&self, position: u32) {
        if let Some(model) = self.imp().model.borrow().as_ref() {
            model.set_selected(position);
        }
        self.scroll_to(position);
    }

    /// Scrolls the enclosing scroller so the item at `position` is visible.
    pub fn scroll_to(&self, position: u32) {
        let Some(scroller) = self.scroller() else { return };
        let adjustment = scroller.vadjustment();
        let width = self.width();
        let columns = self.columns(width);
        let table = self.table(width);
        let row = position as usize / columns.count as usize;
        let Some(&height) = table.heights.get(row) else { return };
        let top = f64::from(table.tops[row]);
        let bottom = top + f64::from(height);
        let page = adjustment.page_size();
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + page {
            adjustment.set_value(bottom - page);
        }
    }

    /// The item at (`x`, `y`) in this grid, if there is one there.
    fn position_at(&self, x: f64, y: f64) -> Option<u32> {
        let items = self.item_count();
        let width = self.width();
        let columns = self.columns(width);
        let table = self.table(width);
        if table.heights.is_empty() {
            return None;
        }
        let row = table.row_at(y);
        if y >= f64::from(table.tops[row] + table.heights[row]) {
            return None;
        }
        let first = (row * columns.count as usize) as u32;
        let in_row = (items - first).min(columns.count as u32);
        (first..first + in_row).find(|&index| {
            let (left, top) = table.origin(columns, index, items, width);
            x >= f64::from(left)
                && x < f64::from(left + columns.cell_w)
                && y >= f64::from(top)
                && y < f64::from(top + table.heights[row])
        })
    }

    fn handle_key(&self, key: gdk::Key) -> glib::Propagation {
        let Some(model) = self.imp().model.borrow().clone() else { return glib::Propagation::Proceed };
        let count = model.n_items();
        if count == 0 {
            return glib::Propagation::Proceed;
        }
        let columns = self.columns(self.width()).count as u32;
        let current = model.selected();
        if key == gdk::Key::Return || key == gdk::Key::KP_Enter || key == gdk::Key::space {
            if current < count {
                self.emit_by_name::<()>("activate", &[&current]);
            }
            return glib::Propagation::Stop;
        }
        // With nothing selected, any movement starts at the first item.
        if current >= count {
            self.select(0);
            return glib::Propagation::Stop;
        }
        let target = if key == gdk::Key::Right {
            (current + 1).min(count - 1)
        } else if key == gdk::Key::Left {
            current.saturating_sub(1)
        } else if key == gdk::Key::Down {
            (current + columns).min(count - 1)
        } else if key == gdk::Key::Up {
            current.saturating_sub(columns)
        } else if key == gdk::Key::Home {
            0
        } else if key == gdk::Key::End {
            count - 1
        } else {
            return glib::Propagation::Proceed;
        };
        self.select(target);
        glib::Propagation::Stop
    }

    /// Connects to `activate`, emitted when an item is double-clicked or confirmed with the keyboard.
    pub fn connect_activate<F: Fn(&Self, u32) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("activate", false, move |values| {
            let grid = values[0].get::<ItemGrid>().expect("grid argument");
            let position = values[1].get::<u32>().expect("position argument");
            callback(&grid, position);
            None
        })
    }

    /// The scroller this grid is in.
    fn scroller(&self) -> Option<gtk::ScrolledWindow> {
        self.ancestor(gtk::ScrolledWindow::static_type()).and_downcast()
    }

    fn columns(&self, width: i32) -> Columns {
        Columns::new(self.cell_size().0, width)
    }

    /// The row heights and positions for a grid `width` wide.
    fn table(&self, width: i32) -> RowTable {
        RowTable::new(
            self.columns(width),
            self.item_count(),
            &self.imp().row_heights.borrow(),
            self.cell_size().1,
        )
    }

    /// The rows on screen, as first and last item positions (inclusive of a row either side, so
    /// scrolling does not reveal blanks).
    fn visible_range(&self, width: i32, count: u32) -> (u32, u32) {
        let (top, page) = match self.scroller() {
            Some(scroller) => {
                let adjustment = scroller.vadjustment();
                (adjustment.value(), adjustment.page_size())
            }
            None => (0.0, f64::from(self.height())),
        };
        let table = self.table(width);
        let columns = self.columns(width).count as usize;
        let rows = table.heights.len();
        if rows == 0 {
            return (0, 0);
        }
        let first_row = table.row_at(top).saturating_sub(1);
        let last_row = (table.row_at(top + page) + 1).min(rows - 1);
        let first = (first_row * columns) as u32;
        let end = (((last_row + 1) * columns) as u32).min(count);
        (first, end)
    }

    /// Makes the shown widgets match the rows on screen: widgets for items that scrolled away are
    /// put aside, and widgets for items that came into view are bound and measured.
    fn update_shown(&self, width: i32) {
        self.follow_scroller();
        let imp = self.imp();
        let Some(model) = imp.model.borrow().clone() else { return };
        let count = model.n_items();
        let columns = self.columns(width);
        // Rows only mean the same items while the number of columns holds.
        if imp.measured_columns.get() != columns.count {
            self.reset();
            imp.measured_columns.set(columns.count);
        }
        let (first, end) = self.visible_range(width, count);

        let gone: Vec<u32> = imp
            .shown
            .borrow()
            .keys()
            .copied()
            .filter(|position| *position < first || *position >= end)
            .collect();
        for position in gone {
            if let Some(widget) = imp.shown.borrow_mut().remove(&position) {
                widget.unparent();
                imp.spare.borrow_mut().push(widget);
            }
        }

        let Some(builders) = imp.builders.borrow().clone() else { return };
        let mut fresh = Vec::new();
        for position in first..end {
            if imp.shown.borrow().contains_key(&position) {
                continue;
            }
            let Some(entry) = model.item(position).and_downcast::<FileEntry>() else { continue };
            let widget = imp.spare.borrow_mut().pop().unwrap_or_else(|| (builders.setup)());
            widget.set_widget_name(&position.to_string());
            widget.set_parent(self);
            widget.set_visible(true);
            (builders.bind)(&widget, &entry, position);
            imp.shown.borrow_mut().insert(position, widget.clone());
            self.set_selected_class(&widget, position);
            fresh.push((position, widget));
        }

        // Each row is as tall as its tallest item: measure the new items and keep the tallest per row.
        let mut changed = false;
        for (position, widget) in fresh {
            let row = position as usize / columns.count as usize;
            let height = widget.measure(gtk::Orientation::Vertical, columns.cell_w).1;
            let mut heights = imp.row_heights.borrow_mut();
            let tallest = heights.entry(row).or_insert(0);
            if height > *tallest {
                *tallest = height;
                changed = true;
            }
        }
        if changed {
            self.queue_resize();
        }
    }

    /// Redraws the selection on the shown items.
    fn update_selected_classes(&self) {
        let selected = self.imp().model.borrow().as_ref().map(|model| model.selected());
        for (position, widget) in self.imp().shown.borrow().iter() {
            widget.remove_css_class("selected");
            if Some(*position) == selected {
                widget.add_css_class("selected");
            }
        }
    }

    fn set_selected_class(&self, widget: &gtk::Widget, position: u32) {
        let selected = self.imp().model.borrow().as_ref().map(|model| model.selected());
        if Some(position) == selected {
            widget.add_css_class("selected");
        }
    }

    /// Removes every shown widget, keeping them to reuse.
    fn clear_children(&self) {
        let imp = self.imp();
        let shown: Vec<gtk::Widget> = imp.shown.borrow_mut().drain().map(|(_, widget)| widget).collect();
        for widget in shown {
            widget.unparent();
            imp.spare.borrow_mut().push(widget);
        }
    }

    /// Listens to the scroller's position, so the shown items follow it.
    fn follow_scroller(&self) {
        let Some(scroller) = self.scroller() else { return };
        let adjustment = scroller.vadjustment();
        let current = self.imp().adjustment.borrow().as_ref().and_then(|(weak, _)| weak.upgrade());
        if current.as_ref() == Some(&adjustment) {
            return;
        }
        let grid = self.downgrade();
        let relayout = move |_: &gtk::Adjustment| {
            if let Some(grid) = grid.upgrade() {
                grid.queue_allocate();
            }
        };
        if let Some((weak, handler)) = self.imp().adjustment.take() {
            if let Some(old) = weak.upgrade() {
                old.disconnect(handler);
            }
        }
        let handler = adjustment.connect_value_changed(relayout.clone());
        adjustment.connect_page_size_notify(move |adjustment| relayout(adjustment));
        self.imp().adjustment.replace(Some((adjustment.downgrade(), handler)));
    }
}
