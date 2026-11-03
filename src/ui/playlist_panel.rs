//! The playlist panel: the items in a table of columns, a search field, and a toolbar of buttons that act on the list. The
//! columns can be resized and hidden, and their contents are ellipsized to fit. A click on a sortable column's heading
//! sorts the playlist by it, and a right click on an item opens its menu. The panel is one widget, which sits beside the
//! browse view or, when detached, in a window of its own. What the panel asks for, it asks of the window through its
//! `win.playlist-*` actions.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use super::drag;
use super::media_viewer::is_video;
use super::playlist_toolbar::{Slot, Toolbar};
use crate::fs::metadata::{anonymous, MediaInfo};
use crate::playlist;
use crate::scripts;

/// How wide the panel is, in pixels.
const PANEL_WIDTH: i32 = 400;
/// The columns in order: the setting that shows each, the menu label that toggles it, its title, and how wide it starts,
/// in pixels. The position column is -1 wide, which is as wide as its marker, and cannot be resized.
pub const COLUMNS: [(&str, &str, &str, i32); 4] = [
    ("playlist-show-position", "Show position column", "#", -1),
    ("playlist-show-name", "Show name column", "Name", -1),
    ("playlist-show-length", "Show length column", "Length", -1),
    ("playlist-show-filename", "Show filename column", "Filename / URL", 300),
];

/// A column the playlist can be sorted by. The position is not one, as it is the order the list is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Length,
    Filename,
}

impl SortColumn {
    /// The column the action that sorts by it names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Length => "length",
            Self::Filename => "filename",
        }
    }

    /// The column named `name` by the action that sorts by it.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "name" => Some(Self::Name),
            "length" => Some(Self::Length),
            "filename" => Some(Self::Filename),
            _ => None,
        }
    }
}

pub struct PlaylistPanel {
    content: gtk::Box,
    view: gtk::ColumnView,
    empty: gtk::Label,
    scroller: gtk::ScrolledWindow,
    /// The items' paths, in the order shown.
    paths: gtk::StringList,
    /// What `paths` holds, to compare with the playlist.
    shown: RefCell<Vec<String>>,
    /// The position of the item marked as the current one.
    current: Rc<Cell<Option<u32>>>,
    /// The number cells showing an item now. Weak, so a cell GTK has let go of is not kept alive here.
    rows: Rc<RefCell<Vec<glib::WeakRef<gtk::ListItem>>>>,
    /// The length cells showing an item now, which are asked for again as the playlist scrolls. Weak, as `rows` is.
    lengths: Rc<RefCell<Vec<glib::WeakRef<gtk::Label>>>>,
    /// The filter that keeps the items the search field matches, and the model it filters.
    filter: gtk::CustomFilter,
    filtered: gtk::FilterListModel,
    /// Each item's path, with its title and file name in lower case, for the search to match against.
    searchable: Rc<RefCell<HashMap<String, String>>>,
}

impl PlaylistPanel {
    /// Makes the panel, showing the playlist as it is. The columns shown and the lengths read follow `settings`.
    pub fn new(settings: &gio::Settings) -> Self {
        eprintln!("playlist-debug: panel new");
        let current = Rc::new(Cell::new(None));
        let rows = Rc::new(RefCell::new(Vec::new()));
        let lengths = Rc::new(RefCell::new(Vec::new()));
        let paths = gtk::StringList::new(&[]);
        let query = Rc::new(RefCell::new(String::new()));
        let searchable: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let filter = gtk::CustomFilter::new({
            let (query, searchable) = (query.clone(), searchable.clone());
            move |object| {
                let query = query.borrow();
                if query.is_empty() {
                    return true;
                }
                let Some(path) = object.downcast_ref::<gtk::StringObject>() else { return false };
                searchable.borrow().get(path.string().as_str()).is_some_and(|text| text.contains(query.as_str()))
            }
        });
        let filtered = gtk::FilterListModel::new(Some(paths.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("data-table");
        view.connect_activate(|view, position| {
            eprintln!("playlist-debug: activate view row {position}");
            // The position is the row's in the view, which the search may have narrowed, so the item is found by its path.
            let path = view.model().and_then(|model| model.item(position)).and_downcast::<gtk::StringObject>();
            let Some(index) = path.and_then(|path| path_position(path.string().as_str())) else { return };
            let _ = view.activate_action("win.playlist-play-index", Some(&index.to_variant()));
        });

        // The sorter of each sortable column compares nothing, so the view keeps the order the playlist is in. Clicking
        // a heading tells the window to sort the playlist instead.
        let mut sortable = Vec::new();
        let saved_widths: Vec<i32> = settings.value("playlist-column-widths").get().unwrap_or_default();
        for (index, (key, _, title, width)) in COLUMNS.into_iter().enumerate() {
            let factory = cell_factory(index, settings, &current, &rows, &lengths);
            let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
            column.set_resizable(width >= 0);
            // The name fills the room between the position and the length, which are sized to their contents.
            column.set_expand(index == 1);
            if width >= 0 {
                column.set_fixed_width(saved_widths.get(index).copied().filter(|saved| *saved > 0).unwrap_or(width));
            }
            settings.bind(key, &column, "visible").build();
            let sort = match index {
                1 => Some(SortColumn::Name),
                2 => Some(SortColumn::Length),
                3 => Some(SortColumn::Filename),
                _ => None,
            };
            if let Some(sort) = sort {
                column.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
                sortable.push((sort, column.clone()));
            }
            column.connect_fixed_width_notify({
                let (view, settings) = (view.downgrade(), settings.clone());
                move |_| {
                    if let Some(view) = view.upgrade() {
                        save_column_widths(&view, &settings);
                    }
                }
            });
            view.append_column(&column);
        }
        // The sort the playlist was left in shows in its column's heading. The playlist is already in that order, so
        // nothing is sorted again.
        let (sort_name, descending): (String, bool) = settings.value("playlist-sort").get().unwrap_or_default();
        if let Some((_, column)) = sortable.iter().find(|(sort, _)| sort.name() == sort_name) {
            let order = if descending { gtk::SortType::Descending } else { gtk::SortType::Ascending };
            view.sort_by_column(Some(column), order);
        }
        if let Some(sorter) = view.sorter() {
            let view_weak = view.downgrade();
            sorter.connect_changed(move |sorter, _| {
                let Some(sorter) = sorter.downcast_ref::<gtk::ColumnViewSorter>() else { return };
                let Some(column) = sorter.primary_sort_column() else { return };
                let Some((sort, _)) = sortable.iter().find(|(_, candidate)| *candidate == column) else { return };
                let descending = sorter.primary_sort_order() == gtk::SortType::Descending;
                if let Some(view) = view_weak.upgrade() {
                    let _ = view.activate_action("win.playlist-sort", Some(&(sort.name(), descending).to_variant()));
                }
            });
        }

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Automatic);
        scroller.set_vscrollbar_policy(gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&view));
        scroller.vadjustment().connect_value_changed({
            let (lengths, settings) = (lengths.clone(), settings.clone());
            move |adjustment| {
                eprintln!("playlist-debug: scroll value={}", adjustment.value());
                bring_visible_lengths_forward(&lengths, settings.boolean("playlist-fetch-in-background"));
            }
        });

        let empty = gtk::Label::new(Some("Nothing yet. Right-click an item to add it, or drag files here."));
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        empty.set_margin_top(6);
        empty.set_margin_bottom(6);
        empty.set_margin_start(6);
        empty.add_css_class("caption");
        empty.add_css_class("dimmed");

        let search = gtk::SearchBar::new();
        search.set_show_close_button(true);
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("Filter the playlist"));
        entry.set_hexpand(true);
        search.set_child(Some(&entry));
        search.connect_entry(&entry);
        entry.connect_search_changed({
            let (query, filter) = (query.clone(), filter.clone());
            move |entry| {
                *query.borrow_mut() = entry.text().to_lowercase();
                filter.changed(gtk::FilterChange::Different);
            }
        });
        // Closing the search shows every item again.
        search.connect_search_mode_enabled_notify({
            let entry = entry.clone();
            move |search| {
                if !search.is_search_mode() {
                    entry.set_text("");
                }
            }
        });

        let toolbar = toolbar(&selection, &view, &search);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("playlist-panel");
        content.set_width_request(PANEL_WIDTH);
        content.append(&empty);
        content.append(&scroller);
        content.append(&search);
        content.append(toolbar.widget());
        content.set_vexpand(true);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(|target, value, _, _| {
            let dropped: Vec<String> = drag::dropped_paths(value).iter().map(|path| path.to_string_lossy().into_owned()).collect();
            let Some(widget) = target.widget() else { return false };
            !dropped.is_empty() && widget.activate_action("win.playlist-add-paths", Some(&dropped.to_variant())).is_ok()
        });
        content.add_controller(drop);

        let panel = Self {
            content,
            view,
            empty,
            scroller,
            paths,
            shown: RefCell::new(Vec::new()),
            current,
            rows,
            lengths,
            filter,
            filtered,
            searchable,
        };
        panel.refresh();
        panel
    }

    /// The panel's widget, which the window puts beside the browse view or in a window of its own.
    pub fn content(&self) -> &gtk::Box {
        &self.content
    }

    /// Shows the playlist's items as they are now, in the order they are shown, with their titles and lengths. Only the
    /// items that changed are added and removed, so the list keeps its place. Every row is bound again, as titles and
    /// lengths may have changed.
    pub fn refresh(&self) {
        eprintln!("playlist-debug: refresh start shown={}", self.shown.borrow().len());
        let (items, searchable) = playlist::with(|list| {
            let items: Vec<String> = list.order().iter().map(|path| path.to_string_lossy().into_owned()).collect();
            let searchable: HashMap<String, String> = list
                .order()
                .iter()
                .map(|path| {
                    let name = path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                    (path.to_string_lossy().into_owned(), format!("{} {name}", list.title(path)).to_lowercase())
                })
                .collect();
            (items, searchable)
        });
        *self.searchable.borrow_mut() = searchable;
        self.filter.changed(gtk::FilterChange::Different);
        let mut shown = self.shown.borrow_mut();
        let start = items.iter().zip(shown.iter()).take_while(|(item, row)| item == row).count();
        let most = items.len().min(shown.len()) - start;
        let end = items.iter().rev().zip(shown.iter().rev()).take(most).take_while(|(item, row)| item == row).count();
        let added: Vec<&str> = items[start..items.len() - end].iter().map(String::as_str).collect();
        eprintln!("playlist-debug: refresh shown={} items={} start={start} end={end} removed={} added={}", shown.len(), items.len(), shown.len() - start - end, added.len());
        self.paths.splice(start as u32, (shown.len() - start - end) as u32, &added);
        self.paths.items_changed(0, items.len() as u32, items.len() as u32);

        self.empty.set_visible(items.is_empty());
        self.scroller.set_visible(!items.is_empty());
        *shown = items;
        eprintln!("playlist-debug: refresh done items={} rows={} lengths={}", self.paths.n_items(), self.rows.borrow().len(), self.lengths.borrow().len());
        drop(shown);
        self.refresh_current();
    }

    /// Marks the playlist's current item, and brings it into view.
    pub fn refresh_current(&self) {
        let current = playlist::with(|list| list.current()).map(|index| index as u32);
        let moved = self.current.replace(current) != current;
        eprintln!("playlist-debug: refresh_current current={current:?} moved={moved} rows={}", self.rows.borrow().len());
        for item in self.rows.borrow().iter().filter_map(|row| row.upgrade()) {
            let Some(position) = playlist_position(&item) else { continue };
            let shown = current == Some(position);
            mark_current_row(&item, shown);
            if let Some(number) = item.child().and_downcast::<gtk::Label>() {
                set_label_if_changed(&number, &number_text(position));
            }
        }
        if let Some(row) = current.filter(|_| moved).and_then(|current| self.view_position(current)) {
            self.view.scroll_to(row, None::<&gtk::ColumnViewColumn>, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// Where the item at playlist position `index` is in the view, or nothing if the search leaves it out.
    fn view_position(&self, index: u32) -> Option<u32> {
        let path = playlist::with(|list| list.order().get(index as usize).map(|path| path.to_string_lossy().into_owned()))?;
        (0..self.filtered.n_items()).find(|row| {
            self.filtered.item(*row).and_downcast::<gtk::StringObject>().is_some_and(|item| item.string().as_str() == path)
        })
    }

    /// The number of rows the panel has.
    #[cfg(test)]
    pub fn row_count(&self) -> u32 {
        self.paths.n_items()
    }

    /// Whether the column at `index`, in the order of `COLUMNS`, is shown.
    #[cfg(test)]
    pub fn column_shown(&self, index: u32) -> bool {
        self.view.columns().item(index).and_downcast::<gtk::ColumnViewColumn>().is_some_and(|column| column.is_visible())
    }
}

/// The number cell's text: the item's place, or the marker for the one shown.
/// Gives the row that `list_item` is in the `current-item` class while `current` is set, so the current item can be styled.
/// A row not in the view yet is left alone, as GTK's only reference to it is still floating: a wrapper around it would take
/// that reference and free the row when dropped. Such a row is marked when it is mapped.
fn mark_current_row(list_item: &gtk::ListItem, current: bool) {
    let Some(child) = list_item.child().filter(|child| child.root().is_some()) else { return };
    let Some(mut row) = child.parent() else { return };
    while row.css_name().as_str() != "row" {
        let Some(parent) = row.parent() else { return };
        row = parent;
    }
    if current {
        row.add_css_class("current-item");
    } else {
        row.remove_css_class("current-item");
    }
}

fn number_text(position: u32) -> String {
    (position + 1).to_string()
}

/// The length a time of `duration` seconds is shown as, `HH:MM:SS`, or `--:--:--` if it is not known.
fn length_text(duration: Option<f64>) -> String {
    let Some(seconds) = duration.filter(|seconds| seconds.is_finite() && *seconds >= 0.0) else { return "--:--:--".to_owned() };
    let total = seconds.round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// The modification time and size of the file at `path`, which its metadata is kept against.
pub fn stamp(path: &Path) -> Option<(i64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((mtime, metadata.len()))
}

/// Sets a label's text only when it differs, so a cell bound again to the same item does not update its text.
fn set_label_if_changed(label: &gtk::Label, text: &str) {
    if label.label().as_str() != text {
        label.set_label(text);
    }
}

/// Shows the length of the item at `path` in `label`. Its metadata is used if it is known. Otherwise, if `fetch`, it is
/// read in the background and shown once it is; if not, it is read when the item is played. The label remembers the
/// path it shows, so a reading that arrives after the row shows another item is dropped.
fn show_length(label: &gtk::Label, path: &Path, fetch: bool) {
    eprintln!("playlist-debug: show_length path={} fetch={fetch}", anonymous(path));
    label.set_widget_name(&path.to_string_lossy());
    set_label_if_changed(&label, "");
    // Only videos have a length, so a picture shows none.
    if !is_video(path) {
        return;
    }
    set_label_if_changed(&label, &length_text(None));
    let Some((mtime, size)) = stamp(path) else { return };
    let info = MediaInfo::shared();
    if !fetch {
        set_label_if_changed(&label, &length_text(info.get(path, mtime, size).and_then(|metadata| metadata.duration)));
        return;
    }
    let shows = Rc::new({
        let weak = label.downgrade();
        let name = path.to_string_lossy().into_owned();
        move || weak.upgrade().is_some_and(|label| label.widget_name().as_str() == name)
    });
    // Only a length on screen is still wanted, so one scrolled away is dropped from the queue until it is back.
    let wanted = {
        let (shows, weak) = (shows.clone(), label.downgrade());
        move || shows() && weak.upgrade().is_some_and(|label| on_screen(&label))
    };
    let ready = {
        let shows = shows.clone();
        let weak = label.downgrade();
        let info = info.clone();
        let path = path.to_path_buf();
        move || {
            if let (true, Some(label)) = (shows(), weak.upgrade()) {
                set_label_if_changed(&label, &length_text(info.get(&path, mtime, size).and_then(|metadata| metadata.duration)));
            }
        }
    };
    let metadata = info.request(path, mtime, size, wanted, ready);
    set_label_if_changed(&label, &length_text(metadata.and_then(|metadata| metadata.duration)));
}

/// Whether `label` is at least partly on screen in the playlist. A label not laid out yet counts as on screen, as it is
/// about to be.
fn on_screen(label: &gtk::Label) -> bool {
    let Some(scroller) = label.ancestor(gtk::ScrolledWindow::static_type()) else { return true };
    let Some(bounds) = label.compute_bounds(&scroller) else { return true };
    bounds.y() < scroller.height() as f32 && bounds.y() + bounds.height() > 0.0
}

/// Brings the lengths on screen forward as the playlist scrolls. One still queued moves to the front of the queue, and
/// one dropped while it was off screen is asked for again.
fn bring_visible_lengths_forward(lengths: &RefCell<Vec<glib::WeakRef<gtk::Label>>>, fetch: bool) {
    let visible: Vec<gtk::Label> = lengths.borrow().iter().filter_map(|label| label.upgrade()).filter(|label| on_screen(label)).collect();
    eprintln!("playlist-debug: visible lengths={} of {}", visible.len(), lengths.borrow().len());
    let info = MediaInfo::shared();
    for label in visible {
        let path = PathBuf::from(label.widget_name().as_str());
        match stamp(&path) {
            Some((mtime, size)) if info.is_pending(&path, mtime, size) => info.prioritise(&path, mtime, size),
            _ => show_length(&label, &path, fetch),
        }
    }
}

/// The factory of one column's cells. Each cell is a label, which can be dragged to move its item, takes items dropped on
/// it, and opens the item's menu on a right click. The number column also keeps the rows it binds, to mark the current one.
fn cell_factory(
    index: usize,
    settings: &gio::Settings,
    current: &Rc<Cell<Option<u32>>>,
    rows: &Rc<RefCell<Vec<glib::WeakRef<gtk::ListItem>>>>,
    lengths: &Rc<RefCell<Vec<glib::WeakRef<gtk::Label>>>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let setup_settings = settings.clone();
    let mapped_current = current.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        eprintln!("playlist-debug: setup column={index}");
        let label = gtk::Label::new(None);
        label.set_margin_start(3);
        label.set_margin_end(3);
        label.set_xalign(match index {
            0 | 2 => 1.0,
            _ => 0.0,
        });
        label.set_ellipsize(match index {
            0 => EllipsizeMode::None,
            3 => EllipsizeMode::Middle,
            _ => EllipsizeMode::End,
        });
        label.set_hexpand(index != 0);
        attach_row_controls(&label, item);
        if index == 0 {
            let (weak, current) = (item.downgrade(), mapped_current.clone());
            label.connect_map(move |_| {
                let Some(item) = weak.upgrade() else { return };
                if let Some(position) = playlist_position(&item) {
                    mark_current_row(&item, current.get() == Some(position));
                }
            });
        }
        item.set_child(Some(&label));
        // The menu listens on the whole row, so a click anywhere on it, padding included, opens it.
        let (weak, menu_settings) = (item.downgrade(), setup_settings.clone());
        label.connect_realize(move |label| {
            let Some(item) = weak.upgrade() else { return };
            // The row spans every column, so only its first cell attaches the menu.
            if index != 0 {
                return;
            }
            let row = std::iter::successors(label.parent(), |widget| widget.parent()).find(|widget| widget.css_name() == "row");
            if let Some(row) = row {
                attach_row_menu(&row, &item, &menu_settings);
            }
        });
    });

    let current = current.clone();
    let bound_rows = rows.clone();
    let unbound_rows = rows.clone();
    let bound_lengths = lengths.clone();
    let unbound_lengths = lengths.clone();
    let bind_settings = settings.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(label), Some(string)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) else {
            return;
        };
        let text = string.string();
        eprintln!("playlist-debug: bind column={index} item={}", anonymous(Path::new(text.as_str())));
        let path = Path::new(text.as_str());
        if index == 0 {
            let mut bound = bound_rows.borrow_mut();
            bound.retain(|row| row.upgrade().is_some());
            if !bound.iter().any(|row| row.upgrade().as_ref() == Some(item)) {
                bound.push(item.downgrade());
            }
        }
        match index {
            0 => {
                if let Some(position) = playlist_position(item) {
                    set_label_if_changed(&label, &number_text(position));
                    mark_current_row(item, current.get() == Some(position));
                }
            }
            1 => {
                set_label_if_changed(&label, &playlist::with(|list| list.title(path)));
                label.set_tooltip_text(Some(path.to_string_lossy().as_ref()));
            }
            2 => {
                let mut bound = bound_lengths.borrow_mut();
                bound.retain(|bound| bound.upgrade().is_some());
                if !bound.iter().any(|bound| bound.upgrade().as_ref() == Some(&label)) {
                    bound.push(label.downgrade());
                }
                drop(bound);
                show_length(&label, path, bind_settings.boolean("playlist-fetch-in-background"));
            }
            _ => set_label_if_changed(&label, &path.to_string_lossy()),
        }
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        eprintln!("playlist-debug: unbind column={index}");
        match index {
            0 => unbound_rows.borrow_mut().retain(|row| row.upgrade().is_some_and(|row| row != *item)),
            2 => {
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    unbound_lengths.borrow_mut().retain(|bound| bound.upgrade().is_some_and(|bound| bound != label));
                }
            }
            _ => {}
        }
    });
    factory
}

/// Gives a cell's label the drag and drop behaviour of its item.
fn attach_row_controls(label: &gtk::Label, item: &gtk::ListItem) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    let weak = item.downgrade();
    source.connect_prepare(move |_, _, _| {
        let position = playlist_position(&weak.upgrade()?)?;
        eprintln!("playlist-debug: drag start from={position}");
        Some(gdk::ContentProvider::for_value(&position.to_value()))
    });
    label.add_controller(source);

    let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
    let weak = item.downgrade();
    target.connect_drop(move |target, value, _, _| {
        let (Some(item), Some(widget), Ok(from)) = (weak.upgrade(), target.widget(), value.get::<u32>()) else { return false };
        let Some(to) = playlist_position(&item) else { return false };
        eprintln!("playlist-debug: drop from={from} to={to}");
        // Moving items changes the order by hand, so the list is no longer sorted by any column.
        if let Some(view) = widget.ancestor(gtk::ColumnView::static_type()).and_downcast::<gtk::ColumnView>() {
            view.sort_by_column(None::<&gtk::ColumnViewColumn>, gtk::SortType::Ascending);
        }
        widget.activate_action("win.playlist-move", Some(&(from, to).to_variant())).is_ok()
    });
    label.add_controller(target);
}

/// Gives a row the menu of its item.
fn attach_row_menu(row: &gtk::Widget, item: &gtk::ListItem, settings: &gio::Settings) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    // The cell's own selection gesture would otherwise get the press first.
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = item.downgrade();
    let menu_settings = settings.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        let (Some(item), Some(widget)) = (weak.upgrade(), gesture.widget()) else { return };
        let Some(path) = item.item().and_downcast::<gtk::StringObject>() else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let Some(position) = playlist_position(&item) else { return };
        eprintln!("playlist-debug: context menu position={position}");
        let menu = row_menu(&menu_settings, position, &path.string());
        // The row lays out only its own cells, so the menu belongs to the view, at the click's place in it.
        let Some(view) = widget.ancestor(gtk::ColumnView::static_type()) else { return };
        let Some(point) = widget.compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32)) else { return };
        popup_menu(&view, &menu, f64::from(point.x()), f64::from(point.y()));
    });
    row.add_controller(click);
}

/// The menu of an item at `position` in the playlist, whose path is `path`: what the item is played and removed by, its
/// file's actions, and which columns are shown.
fn row_menu(settings: &gio::Settings, position: u32, path: &str) -> gio::Menu {
    let item = |label: &str, action: &str, value: glib::Variant| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&value));
        item
    };
    let menu = gio::Menu::new();
    let edit = gio::Menu::new();
    edit.append_item(&item("Play", "win.playlist-play-index", position.to_variant()));
    edit.append_item(&item("Remove item", "win.playlist-remove-index", position.to_variant()));
    edit.append_item(&item("Edit title", "win.playlist-edit-title", position.to_variant()));
    menu.append_section(None, &edit);

    let file = gio::Menu::new();
    file.append_item(&item("Delete from disk…", "win.playlist-delete", path.to_variant()));
    file.append_item(&item("Copy path to clipboard", "win.copy-path", path.to_variant()));
    file.append_item(&item("Reveal in browser", "win.reveal-path", path.to_variant()));
    let open_with = gio::Menu::new();
    for script in scripts::read_scripts(settings) {
        open_with.append_item(&item(&script.name, "win.playlist-open-with", (script.name.clone(), path.to_owned()).to_variant()));
    }
    file.append_submenu(Some("Open with"), &open_with);
    menu.append_section(None, &file);

    let columns = gio::Menu::new();
    for (key, label, _, _) in COLUMNS {
        columns.append(Some(label), Some(&format!("win.{key}")));
    }
    menu.append_section(None, &columns);
    menu
}

/// Pops up `menu` at (`x`, `y`) within `parent`, and takes the popover away when it closes.
fn popup_menu(parent: &gtk::Widget, menu: &gio::Menu, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    // The item's action runs after the menu closes, so the menu is taken away once that is done.
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}


/// Keeps the widths of the playlist's columns, so they open as they were left.
fn save_column_widths(view: &gtk::ColumnView, settings: &gio::Settings) {
    let widths: Vec<i32> = view.columns().iter::<gtk::ColumnViewColumn>().flatten().map(|column| column.fixed_width()).collect();
    let _ = settings.set_value("playlist-column-widths", &widths.to_variant());
}

/// The toolbar under the list. Its buttons act on the playlist, or on the selected item through the actions of the group
/// `playlist`. The search toggle shows the search field above the list, and the buttons that do not fit are collapsed.
fn toolbar(selection: &gtk::SingleSelection, view: &gtk::ColumnView, search: &gtk::SearchBar) -> Toolbar {
    let search_toggle = gtk::ToggleButton::new();
    search_toggle.set_icon_name("system-search-symbolic");
    search_toggle.set_tooltip_text(Some("Search the playlist"));
    search_toggle.add_css_class("flat");
    search.bind_property("search-mode-enabled", &search_toggle, "active").bidirectional().sync_create().build();

    let save = gio::Menu::new();
    save.append(Some("Load"), Some("win.playlist-load"));
    save.append(Some("Save"), Some("win.playlist-save-current"));
    save.append(Some("Save as..."), Some("win.playlist-save"));
    save.append(Some("Load from URL"), Some("win.playlist-load-url"));
    let add = gio::Menu::new();
    add.append(Some("Add current item"), Some("win.playlist-add-current"));
    add.append(Some("Add file(s)"), Some("win.playlist-add-files"));
    add.append(Some("Add directory"), Some("win.playlist-add-directory"));
    let remove = gio::Menu::new();
    remove.append(Some("Remove current item"), Some("win.playlist-remove-current"));
    remove.append(Some("Remove selected"), Some("playlist.remove-selected"));
    remove.append(Some("Remove all"), Some("win.playlist-clear"));

    let slots = vec![
        Slot::Menu { label: "Save / Load", icon: "document-save-symbolic", menu: save },
        Slot::Menu { label: "Add item", icon: "list-add-symbolic", menu: add },
        Slot::Menu { label: "Remove item", icon: "list-remove-symbolic", menu: remove },
        Slot::Button { label: "Play", icon: "media-playback-start-symbolic", action: "playlist.play", toggle: false },
        Slot::Button { label: "Pause", icon: "media-playback-pause-symbolic", action: "win.playlist-pause", toggle: false },
        Slot::Button { label: "Previous Item", icon: "media-skip-backward-symbolic", action: "win.playlist-previous", toggle: false },
        Slot::Button { label: "Next Item", icon: "media-skip-forward-symbolic", action: "win.playlist-next", toggle: false },
        Slot::Button { label: "Repeat", icon: "media-playlist-repeat-symbolic", action: "win.playlist-repeat", toggle: true },
        Slot::Button { label: "Shuffle", icon: "media-playlist-shuffle-symbolic", action: "win.playlist-shuffle", toggle: true },
        Slot::Button { label: "Move item(s) up", icon: "go-up-symbolic", action: "playlist.move-up", toggle: false },
        Slot::Button { label: "Move item(s) down", icon: "go-down-symbolic", action: "playlist.move-down", toggle: false },
        Slot::Button { label: "Move to top", icon: "go-top-symbolic", action: "playlist.move-top", toggle: false },
        Slot::Button { label: "Move to bottom", icon: "go-bottom-symbolic", action: "playlist.move-bottom", toggle: false },
        Slot::Spacer,
    ];
    let toolbar = Toolbar::new(slots, &search_toggle);
    toolbar.widget().insert_action_group("playlist", Some(&selection_actions(selection, view)));
    toolbar
}

/// Runs a window action from `view`, which the toolbar's buttons are in.
fn activate(view: &gtk::ColumnView, name: &str, parameter: Option<&glib::Variant>) {
    let _ = view.activate_action(name, parameter);
}

/// The number of items in the playlist.
fn playlist_len() -> u32 {
    playlist::with(|list| list.len() as u32)
}

/// Whether a selection action is enabled, given the playlist position of the selected item, if any, and the playlist's length.
type Needs = fn(Option<u32>, u32) -> bool;
/// Runs a selection action, given the view it runs from, the selected item's position, and the playlist's length.
type Runs = fn(&gtk::ColumnView, Option<u32>, u32);

/// The actions on the selected item, in the group `playlist` that the toolbar's buttons and its menu run. Each is enabled
/// only while there is something for it to act on.
fn selection_actions(selection: &gtk::SingleSelection, view: &gtk::ColumnView) -> gio::SimpleActionGroup {
    let specs: [(&str, Needs, Runs); 6] = [
        ("play", |_, _| true, |view, index, _| match index {
            Some(index) => activate(view, "win.playlist-play-index", Some(&index.to_variant())),
            None => activate(view, "win.playlist-play", None),
        }),
        ("remove-selected", |index, _| index.is_some(), |view, index, _| {
            if let Some(index) = index {
                activate(view, "win.playlist-remove-index", Some(&index.to_variant()));
            }
        }),
        ("move-up", |index, _| index.is_some_and(|from| from > 0), |view, index, _| {
            if let Some(from) = index {
                activate(view, "win.playlist-move", Some(&(from, from - 1).to_variant()));
            }
        }),
        ("move-down", |index, len| index.is_some_and(|from| from + 1 < len), |view, index, _| {
            if let Some(from) = index {
                activate(view, "win.playlist-move", Some(&(from, from + 1).to_variant()));
            }
        }),
        ("move-top", |index, _| index.is_some_and(|from| from > 0), |view, index, _| {
            if let Some(from) = index {
                activate(view, "win.playlist-move", Some(&(from, 0u32).to_variant()));
            }
        }),
        ("move-bottom", |index, len| index.is_some_and(|from| from + 1 < len), |view, index, len| {
            if let Some(from) = index {
                activate(view, "win.playlist-move", Some(&(from, len - 1).to_variant()));
            }
        }),
    ];
    let group = gio::SimpleActionGroup::new();
    let mut actions = Vec::new();
    for (name, needs, runs) in specs {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate({
            let (selection, view) = (selection.clone(), view.clone());
            move |_, _| {
                eprintln!("playlist-debug: action {name} selected={:?} len={}", selected(&selection), playlist_len());
                runs(&view, selected(&selection), playlist_len())
            }
        });
        group.add_action(&action);
        actions.push((action, needs));
    }
    let refresh: Rc<dyn Fn()> = Rc::new({
        let selection = selection.clone();
        move || {
            let index = selected(&selection);
            let len = playlist_len();
            for (action, needs) in &actions {
                action.set_enabled(needs(index, len));
            }
        }
    });
    refresh();
    let on_items = refresh.clone();
    selection.connect_selected_notify(move |_| refresh());
    selection.connect_items_changed(move |_, _, _, _| on_items());
    group
}

/// The playlist position of the item `list_item` shows. The search can leave items out of the view, so a row's own
/// position there is not the item's.
fn playlist_position(list_item: &gtk::ListItem) -> Option<u32> {
    let path = list_item.item().and_downcast::<gtk::StringObject>()?.string();
    path_position(path.as_str())
}

/// The playlist position of the item at `path`, if it is in the playlist.
fn path_position(path: &str) -> Option<u32> {
    playlist::with(|list| list.order().iter().position(|item| item.as_path() == Path::new(path))).map(|index| index as u32)
}

/// The playlist position of the selected item, if one is selected.
fn selected(selection: &gtk::SingleSelection) -> Option<u32> {
    let path = selection.selected_item().and_downcast::<gtk::StringObject>()?.string();
    path_position(path.as_str())
}
