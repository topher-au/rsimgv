//! The grid of browse items. It is a plain widget with its own layout: each item keeps its width,
//! rows are filled left to right and the block of rows is centred, so every row starts at the same edge.
//! Every item in a row is as tall as the tallest one there, and only the items on screen have widgets.
//!
//! It sits in a `GtkScrolledWindow`, which does the scrolling. The grid is as tall as all its rows,
//! so the scroller offsets it; the grid reads the scroller's adjustment to know what is visible.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::fs::FileEntry;

/// Space between items horizontally and vertically, in pixels.
const SPACING_X: i32 = 8;
const SPACING_Y: i32 = 8;
/// The most layouts the view is put back after a reflow over, in case the rows never settle.
const MAX_RESTORE_PASSES: u8 = 20;

/// The position `delta` items on from `current`, kept within `count` items. With nothing selected, any
/// move starts at the first item.
fn target_position(current: u32, count: u32, delta: i64) -> u32 {
    if current >= count {
        return 0;
    }
    (i64::from(current) + delta).clamp(0, i64::from(count) - 1) as u32
}

/// Adds or removes a CSS class on a widget.
fn set_class(widget: &gtk::Widget, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Builds an item's widget once, fills it in for an item each time it is shown, and clears it when it stops.
pub struct Builders {
    setup: Box<dyn Fn() -> gtk::Widget>,
    bind: Box<dyn Fn(&gtk::Widget, &FileEntry, u32)>,
    unbind: Box<dyn Fn(&gtk::Widget)>,
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
    /// The column count and item count this table is for.
    count: i32,
    items: u32,
    /// Where each row starts, with one extra entry for the end of the last row.
    tops: Vec<i32>,
    heights: Vec<i32>,
}

impl RowTable {
    /// The table for `items` in `columns`. `measured` holds each row's measured height, with 0 for a row
    /// not measured yet, which uses `default`.
    fn new(columns: Columns, items: u32, measured: &[i32], default: i32) -> Self {
        let rows = columns.rows(items);
        let heights: Vec<i32> = (0..rows)
            .map(|row| match measured.get(row) {
                Some(&height) if height > 0 => height,
                _ => default,
            })
            .collect();
        let mut tops = Vec::with_capacity(rows + 1);
        let mut top = 0;
        for height in &heights {
            tops.push(top);
            top += height + SPACING_Y;
        }
        tops.push(top);
        Self {
            count: columns.count,
            items,
            tops,
            heights,
        }
    }

    /// Whether this table is for `columns` and `items`, so it can be used as it is.
    fn is_for(&self, columns: Columns, items: u32) -> bool {
        self.count == columns.count && self.items == items
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

    /// The top-left corner of the item at `index` in a grid `width` wide. Every row starts at the same
    /// x: the rows are left-aligned within a block of full rows, and that block is centred.
    fn origin(&self, columns: Columns, index: u32, width: i32) -> (i32, i32) {
        let row = index as usize / columns.count as usize;
        let col = index as i32 % columns.count;
        let block_width = columns.count * columns.cell_w + (columns.count - 1) * SPACING_X;
        let x0 = ((width - block_width) / 2).max(0);
        (x0 + col * (columns.cell_w + SPACING_X), self.tops[row])
    }
}

/// Where the view stands: an item, and how far the item's row starts below the top of the view (negative for a row that
/// starts above it). Kept so the item holds its place when the rows are laid out again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewAnchor {
    position: u32,
    offset: f64,
}

/// The anchor for a view `value` pixels down and `page` tall over `table`: the selected item if it is entirely in
/// view, otherwise the first item of the row at the top.
fn anchor_for(table: &RowTable, columns: i32, items: u32, selected: Option<u32>, value: f64, page: f64) -> Option<ViewAnchor> {
    if items == 0 || table.heights.is_empty() {
        return None;
    }
    if let Some(position) = selected.filter(|position| *position < items) {
        let row = position as usize / columns as usize;
        let (top, bottom) = (f64::from(table.tops[row]), f64::from(table.tops[row] + table.heights[row]));
        if top >= value && bottom <= value + page {
            return Some(ViewAnchor { position, offset: top - value });
        }
    }
    let row = table.row_at(value);
    Some(ViewAnchor { position: (row * columns as usize) as u32, offset: f64::from(table.tops[row]) - value })
}

/// How far down the view must be so `anchor`'s item is as far below the top of the view as it was, over `table`.
fn value_for(table: &RowTable, columns: i32, items: u32, anchor: ViewAnchor) -> Option<f64> {
    if items == 0 {
        return None;
    }
    let row = anchor.position.min(items - 1) as usize / columns as usize;
    table.tops.get(row).map(|top| f64::from(*top) - anchor.offset)
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
            let changed = grid.update_shown(width);
            let columns = grid.columns(width);
            let table = grid.table(width);
            for (&index, cell) in grid.imp().shown.borrow().iter() {
                let row = index as usize / columns.count as usize;
                let (x, y) = table.origin(columns, index, width);
                let transform = gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32));
                cell.allocate(columns.cell_w, table.heights[row], -1, Some(transform));
            }
            if let Some(anchor) = grid.imp().restore.get() {
                // A selection to scroll to takes the view's place instead.
                if grid.imp().reveal.get().is_some() {
                    grid.imp().restore.take();
                } else {
                    grid.restore_view(anchor, width, &table, changed);
                }
            }
            if grid.imp().restore.get().is_none() && !changed {
                grid.remember_view(width, &table);
            }
            // A selection made while the grid was hidden, or before it had its size, is scrolled to now. If rows were
            // just measured, the scroller does not know the grid's new height yet, so the scroll waits for the next
            // layout; otherwise a row that grew would leave the bottom of the list out of reach.
            if !changed {
                if let Some(position) = grid.imp().reveal.take() {
                    grid.scroll_to(position);
                }
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
        /// Widgets no longer shown, kept to be reused. They stay children of the grid, hidden.
        pub(super) spare: RefCell<Vec<gtk::Widget>>,
        /// The first and end item positions the shown widgets were made for, once they are up to date.
        pub(super) shown_range: Cell<Option<(u32, u32)>>,
        /// The position to scroll into view at the next layout, once the geometry is known.
        pub(super) reveal: Cell<Option<u32>>,
        /// The item width and the default item height, in pixels.
        pub(super) cell_size: Cell<(i32, i32)>,
        /// The measured height of each row, by row number, or 0 for a row not measured yet. These are for
        /// the columns in `measured_columns`.
        pub(super) row_heights: RefCell<Vec<i32>>,
        pub(super) measured_columns: Cell<i32>,
        /// The row layout, kept until the columns, the items or a measured height change.
        pub(super) table: RefCell<Option<Rc<RowTable>>>,
        /// The scroller's adjustment this grid listens to, and the handler on it.
        pub(super) adjustment: RefCell<Option<(glib::WeakRef<gtk::Adjustment>, glib::SignalHandlerId)>>,
        /// The selection whose changes are followed, so they are connected once.
        pub(super) connected: RefCell<Option<glib::WeakRef<gtk::SingleSelection>>>,
        /// The items marked for acting on together, by path, so a mark follows its item through a relayout.
        pub(super) marked: RefCell<HashSet<PathBuf>>,
        /// Whether a batch of changes to the items is being made, in which the marks are not pruned item by item.
        pub(super) batching: Cell<bool>,
        /// The position a shift-click extends the marks from: where the last plain or ctrl-click, or the
        /// last move of the cursor, left the cursor.
        pub(super) anchor: Cell<Option<u32>>,
        /// Where the view stood at the last layout that left the rows as they were.
        pub(super) view: Cell<Option<ViewAnchor>>,
        /// Where the view is to be put back to after the rows were laid out again with another number of columns, until
        /// the scroller has taken the new height of the grid, and how many layouts that has taken.
        pub(super) restore: Cell<Option<ViewAnchor>>,
        pub(super) restore_passes: Cell<u8>,
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
                    let modifiers = gesture.current_event_state();
                    let Some(position) = grid.position_at(x, y) else {
                        if !modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK) {
                            grid.clear_marks();
                        }
                        return;
                    };
                    if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                        grid.mark_range_to(position);
                    } else if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                        grid.toggle_mark(position);
                    } else {
                        grid.clear_marks();
                        grid.select(position);
                        if n_press == 2 {
                            grid.emit_by_name::<()>("activate", &[&position]);
                        }
                    }
                }
            });
            obj.add_controller(click);
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> = LazyLock::new(|| {
                vec![
                    glib::subclass::Signal::builder("activate")
                        .param_types([u32::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("marks-changed").build(),
                ]
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
    /// for the item at a position; `unbind` clears it when it stops showing an item and is put aside.
    pub fn set_builders(
        &self,
        setup: impl Fn() -> gtk::Widget + 'static,
        bind: impl Fn(&gtk::Widget, &FileEntry, u32) + 'static,
        unbind: impl Fn(&gtk::Widget) + 'static,
    ) {
        self.imp().builders.replace(Some(Rc::new(Builders {
            setup: Box::new(setup),
            bind: Box::new(bind),
            unbind: Box::new(unbind),
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
            move |_, position, _, _| {
                if let Some(grid) = grid.upgrade() {
                    grid.hold_view();
                    grid.reset_from(position);
                    grid.prune_marks();
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

    /// Has the next layout put the view back where it stands now. Forgetting row heights shrinks the grid to its
    /// estimate until the rows are measured again, and the scroller would clamp the scroll position, possibly to the top.
    fn hold_view(&self) {
        let imp = self.imp();
        if imp.model.borrow().is_none() || imp.restore.get().is_some() {
            return;
        }
        imp.restore.set(imp.view.get());
        imp.restore_passes.set(0);
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
        self.imp().table.take();
    }

    /// Removes the shown items from `position` on, and forgets the row heights of the rows from there on. The
    /// items before it did not change, so their widgets and heights stay.
    fn reset_from(&self, position: u32) {
        let imp = self.imp();
        let columns = imp.measured_columns.get().max(1) as usize;
        let from_row = position as usize / columns;
        let gone: Vec<u32> = imp.shown.borrow().keys().copied().filter(|shown| *shown >= position).collect();
        for shown in gone {
            let removed = imp.shown.borrow_mut().remove(&shown);
            if let Some(widget) = removed {
                self.put_aside(widget);
            }
        }
        imp.row_heights.borrow_mut().truncate(from_row);
        imp.shown_range.set(None);
        imp.table.take();
    }

    /// Makes a batch of changes to the items. The marks follow their items through it, and are pruned once, at its end.
    pub fn batch_changes(&self, change: impl FnOnce()) {
        let imp = self.imp();
        imp.batching.set(true);
        change();
        imp.batching.set(false);
        self.prune_marks();
    }

    /// Selects the item at `position`, and scrolls it fully into view. The scroll happens at the grid's next layout,
    /// so it uses the grid's real size, even when the grid is hidden until then.
    pub fn select(&self, position: u32) {
        if let Some(model) = self.imp().model.borrow().as_ref() {
            model.set_selected(position);
        }
        self.imp().anchor.set(Some(position));
        self.imp().reveal.set(Some(position));
        self.queue_allocate();
    }

    /// How many items are marked. Marks are pruned as the items change, so each one is a listed item.
    pub fn marked_count(&self) -> usize {
        self.imp().marked.borrow().len()
    }

    /// The paths of the marked items, in the order they are listed.
    pub fn marked_paths(&self) -> Vec<PathBuf> {
        let marked = self.imp().marked.borrow();
        if marked.is_empty() {
            return Vec::new();
        }
        (0..self.item_count()).filter_map(|position| self.path_at(position)).filter(|path| marked.contains(path)).collect()
    }

    /// Marks every item, as a select-all does.
    pub fn mark_all(&self) {
        let paths: HashSet<PathBuf> = (0..self.item_count()).filter_map(|position| self.path_at(position)).collect();
        if !paths.is_empty() {
            self.imp().marked.replace(paths);
            self.marks_changed();
        }
    }

    /// Unmarks every item.
    pub fn clear_marks(&self) {
        if !self.imp().marked.borrow().is_empty() {
            self.imp().marked.borrow_mut().clear();
            self.marks_changed();
        }
    }

    /// Marks the item at `position` if it is not marked, and unmarks it if it is, as ctrl-click does. The
    /// cursor moves to it.
    fn toggle_mark(&self, position: u32) {
        if let Some(path) = self.path_at(position) {
            let mut marked = self.imp().marked.borrow_mut();
            if !marked.remove(&path) {
                marked.insert(path);
            }
            drop(marked);
            self.marks_changed();
        }
        self.select(position);
    }

    /// Marks the items from the anchor to `position`, in place of the marks before, as shift-click does. The
    /// cursor moves to `position`, and the anchor stays where it was.
    fn mark_range_to(&self, position: u32) {
        let anchor = self.imp().anchor.get().unwrap_or(position);
        let (first, last) = (anchor.min(position), anchor.max(position));
        let paths: HashSet<PathBuf> = (first..=last).filter_map(|index| self.path_at(index)).collect();
        self.imp().marked.replace(paths);
        self.marks_changed();
        self.select(position);
        self.imp().anchor.set(Some(anchor));
    }

    /// Drops the marks of items that are no longer listed, as when the folder changes.
    fn prune_marks(&self) {
        if self.imp().batching.get() || self.imp().marked.borrow().is_empty() {
            return;
        }
        let listed: HashSet<PathBuf> = (0..self.item_count()).filter_map(|position| self.path_at(position)).collect();
        let before = self.imp().marked.borrow().len();
        self.imp().marked.borrow_mut().retain(|path| listed.contains(path));
        if self.imp().marked.borrow().len() != before {
            self.marks_changed();
        }
    }

    /// Redraws the marks and tells the owner they changed.
    fn marks_changed(&self) {
        self.update_selected_classes();
        self.emit_by_name::<()>("marks-changed", &[]);
    }

    /// Connects to `marks-changed`, emitted when items are marked or unmarked.
    pub fn connect_marks_changed<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("marks-changed", false, move |values| {
            let grid = values[0].get::<ItemGrid>().expect("grid argument");
            callback(&grid);
            None
        })
    }

    /// The path of the item at `position`.
    fn path_at(&self, position: u32) -> Option<PathBuf> {
        let model = self.imp().model.borrow().clone()?;
        model.item(position).and_downcast::<FileEntry>().map(|entry| entry.path().to_path_buf())
    }

    /// Whether the item at `position` is marked.
    fn is_marked_at(&self, position: u32) -> bool {
        !self.imp().marked.borrow().is_empty() && self.path_at(position).is_some_and(|path| self.imp().marked.borrow().contains(&path))
    }

    /// The widget showing the item at `position`, if it is on screen.
    pub fn cell_at(&self, position: u32) -> Option<gtk::Widget> {
        self.imp().shown.borrow().get(&position).cloned()
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
            let (left, top) = table.origin(columns, index, width);
            x >= f64::from(left)
                && x < f64::from(left + columns.cell_w)
                && y >= f64::from(top)
                && y < f64::from(top + table.heights[row])
        })
    }

    /// Moves the selection to the first item.
    pub fn select_first(&self) {
        self.select(0);
    }

    /// Selects the item at `position` without scrolling to it, for a view that keeps its scroll position.
    pub fn select_in_place(&self, position: u32) {
        if let Some(model) = self.imp().model.borrow().as_ref() {
            model.set_selected(position);
        }
        self.imp().anchor.set(Some(position));
    }

    /// Moves the selection to the last item.
    pub fn select_last(&self) {
        let count = self.item_count();
        if count > 0 {
            self.select(count - 1);
        }
    }

    /// Opens the selected item, as if it had been double-clicked.
    pub fn activate_selected(&self) {
        let Some(model) = self.imp().model.borrow().clone() else { return };
        let current = model.selected();
        if current < model.n_items() {
            self.emit_by_name::<()>("activate", &[&current]);
        }
    }

    /// Moves the selection `items` along the list: negative moves back. Moves are clamped to the grid.
    pub fn move_cursor_x(&self, items: i32) {
        self.move_cursor(i64::from(items));
    }

    /// Moves the selection `rows` rows down the grid: negative moves up.
    pub fn move_cursor_y(&self, rows: i32) {
        let columns = i64::from(self.columns(self.width()).count);
        self.move_cursor(i64::from(rows) * columns);
    }

    /// Moves the selection `pages` pages down the grid, where a page is the rows the scroller shows.
    pub fn move_cursor_page(&self, pages: i32) {
        let row_height = f64::from(self.cell_size().1 + SPACING_Y);
        let rows = (self.page_height() / row_height).floor().max(1.0) as i32;
        self.move_cursor_y(pages * rows);
    }

    fn move_cursor(&self, delta: i64) {
        let Some(model) = self.imp().model.borrow().clone() else { return };
        let count = model.n_items();
        if count == 0 {
            return;
        }
        self.select(target_position(model.selected(), count, delta));
    }

    /// The height of the part of the grid the scroller shows.
    fn page_height(&self) -> f64 {
        self.scroller().map_or(f64::from(self.height()), |scroller| scroller.vadjustment().page_size())
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

    /// The row heights and positions for a grid `width` wide. The table is reused until the columns or the
    /// items change, or a row is measured again.
    fn table(&self, width: i32) -> Rc<RowTable> {
        let columns = self.columns(width);
        let items = self.item_count();
        let mut cached = self.imp().table.borrow_mut();
        if let Some(table) = cached.as_ref().filter(|table| table.is_for(columns, items)) {
            return table.clone();
        }
        let table = Rc::new(RowTable::new(
            columns,
            items,
            &self.imp().row_heights.borrow(),
            self.cell_size().1,
        ));
        *cached = Some(table.clone());
        table
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
    /// Shows the items in view and measures them. Returns whether a row's height changed, which means the grid is
    /// about to be laid out again.
    fn update_shown(&self, width: i32) -> bool {
        self.follow_scroller();
        let imp = self.imp();
        let Some(model) = imp.model.borrow().clone() else { return false };
        let count = model.n_items();
        let columns = self.columns(width);
        let mut changed = false;
        // Rows only mean the same items while the number of columns holds.
        if imp.measured_columns.get() != columns.count {
            // The view is put back where it stood once the rows are laid out for the new columns. A reflow already
            // waiting for that keeps the place it began from.
            if imp.restore.get().is_none() {
                imp.restore.set(imp.view.get());
                imp.restore_passes.set(0);
            }
            self.reset();
            imp.measured_columns.set(columns.count);
            changed = true;
        }
        let (first, end) = self.visible_range(width, count);
        // Scrolling within the same rows shows the same items, so there is nothing to change.
        if imp.shown_range.get() == Some((first, end)) {
            return changed;
        }

        let gone: Vec<u32> = imp
            .shown
            .borrow()
            .keys()
            .copied()
            .filter(|position| *position < first || *position >= end)
            .collect();
        for position in gone {
            // Taken out first: hiding a cell can run handlers that look at the shown cells.
            let removed = imp.shown.borrow_mut().remove(&position);
            if let Some(widget) = removed {
                self.put_aside(widget);
            }
        }

        let Some(builders) = imp.builders.borrow().clone() else { return changed };
        let mut fresh = Vec::new();
        for position in first..end {
            if imp.shown.borrow().contains_key(&position) {
                continue;
            }
            let Some(entry) = model.item(position).and_downcast::<FileEntry>() else { continue };
            let spare = imp.spare.borrow_mut().pop();
            let widget = match spare {
                Some(widget) => {
                    widget.set_child_visible(true);
                    widget
                }
                None => {
                    let widget = (builders.setup)();
                    widget.set_parent(self);
                    widget
                }
            };
            widget.set_widget_name(&position.to_string());
            widget.set_visible(true);
            (builders.bind)(&widget, &entry, position);
            imp.shown.borrow_mut().insert(position, widget.clone());
            self.set_item_classes(&widget, position);
            fresh.push((position, widget));
        }
        imp.shown_range.set(Some((first, end)));

        // Each row is as tall as its tallest item: measure the new items and keep the tallest per row.
        for (position, widget) in fresh {
            let row = position as usize / columns.count as usize;
            let height = widget.measure(gtk::Orientation::Vertical, columns.cell_w).1;
            let mut heights = imp.row_heights.borrow_mut();
            if heights.len() <= row {
                heights.resize(row + 1, 0);
            }
            if height > heights[row] {
                heights[row] = height;
                changed = true;
            }
        }
        if changed {
            imp.table.take();
            self.queue_resize();
        }

        // Keep about two screens of spare widgets for the next scroll, and let the rest go.
        let keep = (2 * end.saturating_sub(first)) as usize;
        let surplus: Vec<gtk::Widget> = {
            let mut spare = imp.spare.borrow_mut();
            if spare.len() > keep {
                spare.split_off(keep)
            } else {
                Vec::new()
            }
        };
        for widget in surplus {
            widget.unparent();
        }
        changed
    }

    /// Redraws the selection, the marks and what is in the playlist on the shown items.
    pub fn update_selected_classes(&self) {
        for (position, widget) in self.imp().shown.borrow().iter() {
            self.set_item_classes(widget, *position);
        }
    }

    /// Sets the `selected`, `marked` and `in-playlist` classes of a widget showing the item at `position`. A reused
    /// widget may still have the classes of the item it showed before, so all are set either way.
    fn set_item_classes(&self, widget: &gtk::Widget, position: u32) {
        let selected = self.imp().model.borrow().as_ref().map(|model| model.selected());
        set_class(widget, "selected", Some(position) == selected);
        set_class(widget, "marked", self.is_marked_at(position));
        set_class(widget, "in-playlist", self.path_at(position).is_some_and(|path| crate::playlist::contains(&path)));
    }

    /// Removes every shown widget, keeping them to reuse.
    fn clear_children(&self) {
        let imp = self.imp();
        imp.shown_range.set(None);
        let shown: Vec<gtk::Widget> = imp.shown.borrow_mut().drain().map(|(_, widget)| widget).collect();
        for widget in shown {
            self.put_aside(widget);
        }
    }

    /// Keeps a widget that is no longer shown to reuse, cleared of its item.
    fn put_aside(&self, widget: gtk::Widget) {
        let imp = self.imp();
        put_spare(&widget);
        let builders = imp.builders.borrow().clone();
        if let Some(builders) = builders {
            (builders.unbind)(&widget);
        }
        imp.spare.borrow_mut().push(widget);
    }

    /// Notes where the view stands: the selected item if it is in view, otherwise the item at the top.
    fn remember_view(&self, width: i32, table: &RowTable) {
        let imp = self.imp();
        let Some(scroller) = self.scroller() else { return };
        let adjustment = scroller.vadjustment();
        let selected = imp.model.borrow().as_ref().map(|model| model.selected());
        let anchor = anchor_for(table, self.columns(width).count, self.item_count(), selected, adjustment.value(), adjustment.page_size());
        imp.view.set(anchor);
    }

    /// Puts the view back where `anchor` says, over the rows as they are now. That is repeated at each layout until
    /// the rows have been measured and the scroller has the grid's height, which a scroll cannot go beyond before.
    fn restore_view(&self, anchor: ViewAnchor, width: i32, table: &RowTable, measuring: bool) {
        let imp = self.imp();
        let Some(scroller) = self.scroller() else {
            imp.restore.take();
            return;
        };
        let adjustment = scroller.vadjustment();
        let Some(wanted) = value_for(table, self.columns(width).count, self.item_count(), anchor) else {
            imp.restore.take();
            return;
        };
        let reachable = wanted.clamp(adjustment.lower(), (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()));
        adjustment.set_value(wanted);
        let passes = imp.restore_passes.get().saturating_add(1);
        imp.restore_passes.set(passes);
        let known = adjustment.upper() + 0.5 >= f64::from(table.total());
        if (!measuring && known && (adjustment.value() - reachable).abs() < 0.5) || passes >= MAX_RESTORE_PASSES {
            imp.restore.take();
        } else {
            self.queue_allocate();
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

#[cfg(test)]
mod tests {
    use super::{anchor_for, target_position, value_for, Columns, RowTable, ViewAnchor};

    fn table(columns: i32, items: u32) -> (Columns, RowTable) {
        let columns = Columns { cell_w: 100, count: columns };
        (columns, RowTable::new(columns, items, &[], 92))
    }

    #[test]
    fn the_item_at_the_top_keeps_its_place_when_the_columns_change() {
        // Four columns, scrolled to the middle of the fourth row (item 12 starts it).
        let (four, rows_of_four) = table(4, 100);
        let value = f64::from(rows_of_four.tops[3]) + 30.0;
        let anchor = anchor_for(&rows_of_four, four.count, 100, None, value, 400.0).unwrap();
        assert_eq!(anchor, ViewAnchor { position: 12, offset: -30.0 });

        // Five columns put item 12 in the third row, and the view is where that row starts less the same 30.
        let (five, rows_of_five) = table(5, 100);
        let restored = value_for(&rows_of_five, five.count, 100, anchor).unwrap();
        assert_eq!(restored, f64::from(rows_of_five.tops[2]) + 30.0);
    }

    #[test]
    fn a_selected_item_in_view_holds_its_place_over_the_top_one() {
        let (four, rows) = table(4, 100);
        let value = f64::from(rows.tops[3]);
        // Item 21 is in the sixth row, which is in view in a page of 400.
        let anchor = anchor_for(&rows, four.count, 100, Some(21), value, 400.0).unwrap();
        assert_eq!(anchor.position, 21);
        assert_eq!(anchor.offset, f64::from(rows.tops[5] - rows.tops[3]));
        // One out of view is not held.
        let anchor = anchor_for(&rows, four.count, 100, Some(90), value, 400.0).unwrap();
        assert_eq!(anchor.position, 12);
    }

    #[test]
    fn no_items_have_no_place_to_keep() {
        let (columns, rows) = table(4, 0);
        assert_eq!(anchor_for(&rows, columns.count, 0, None, 0.0, 400.0), None);
        assert_eq!(value_for(&rows, columns.count, 0, ViewAnchor { position: 3, offset: 0.0 }), None);
    }

    #[test]
    fn an_anchor_past_the_end_of_a_shorter_list_is_held_at_the_last_item() {
        let (columns, rows) = table(4, 10);
        let value = value_for(&rows, columns.count, 10, ViewAnchor { position: 50, offset: 0.0 }).unwrap();
        assert_eq!(value, f64::from(rows.tops[2]));
    }

    #[test]
    fn moves_are_clamped_to_the_list() {
        assert_eq!(target_position(5, 10, 1), 6);
        assert_eq!(target_position(5, 10, -3), 2);
        assert_eq!(target_position(1, 10, -4), 0);
        assert_eq!(target_position(8, 10, 9), 9);
    }

    #[test]
    fn a_move_with_nothing_selected_starts_at_the_first_item() {
        assert_eq!(target_position(u32::MAX, 10, 3), 0);
    }
}

/// Hides a widget that is no longer shown, and forgets the item it showed: its name is what says which item a widget
/// shows, so a spare widget must not still claim one.
fn put_spare(widget: &gtk::Widget) {
    widget.set_child_visible(false);
    widget.set_widget_name("");
}
