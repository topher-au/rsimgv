//! The playlist panel: a heading, the items in columns under a row of headings, and a toolbar of buttons that act on the
//! list. A click on a sortable heading sorts the playlist by that column, and a right click on an item opens its menu.
//! The columns can be hidden from that menu. The panel is one widget, which sits beside the browse view or, when
//! detached, in a window of its own. What the panel asks for, it asks of the window through its `win.playlist-*` actions.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use super::drag;
use crate::fs::metadata::MediaInfo;
use crate::playlist;
use crate::scripts;

/// How wide the panel is, in pixels.
const PANEL_WIDTH: i32 = 400;
/// The width of the position column, and of the length column, which holds a time.
const POSITION_WIDTH: i32 = 36;
const LENGTH_WIDTH: i32 = 72;
/// The symbol that marks the item shown, in the position column.
const CURRENT_MARK: &str = "▶";
/// The setting that shows each column, and the menu label that toggles it, in the order the columns are in: position,
/// name, length, filename.
const COLUMNS: [(&str, &str); 4] = [
    ("playlist-show-position", "Show position column"),
    ("playlist-show-name", "Show name column"),
    ("playlist-show-length", "Show length column"),
    ("playlist-show-filename", "Show filename column"),
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

    fn heading(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Length => "Length",
            Self::Filename => "Filename / URL",
        }
    }
}

/// The sortable headings, each with its label, and the column the list is sorted by, and whether it is sorted down.
struct Headings {
    buttons: Vec<(SortColumn, gtk::Button, gtk::Label)>,
    sorted: Cell<Option<(SortColumn, bool)>>,
}

impl Headings {
    /// Shows the arrow of the column the list is sorted by, and no arrow on the others.
    fn show(&self) {
        for (column, _, label) in &self.buttons {
            let arrow = match self.sorted.get() {
                Some((sorted, false)) if sorted == *column => " ▲",
                Some((sorted, true)) if sorted == *column => " ▼",
                _ => "",
            };
            label.set_label(&format!("{}{arrow}", column.heading()));
        }
    }
}

pub struct PlaylistPanel {
    content: gtk::Box,
    /// The heading, which says how many items there are.
    title: gtk::Label,
    detach: gtk::Button,
    shuffle: gtk::Button,
    header: gtk::Box,
    view: gtk::ListView,
    empty: gtk::Label,
    scroller: gtk::ScrolledWindow,
    /// The items' paths, in the order shown.
    paths: gtk::StringList,
    /// What `paths` holds, to compare with the playlist.
    shown: RefCell<Vec<String>>,
    /// The position of the item marked as the current one.
    current: Rc<Cell<Option<u32>>>,
    /// The rows showing an item now.
    rows: Rc<RefCell<Vec<gtk::ListItem>>>,
}

impl PlaylistPanel {
    /// Makes the panel, showing the playlist as it is. The columns shown follow `settings`.
    pub fn new(settings: &gio::Settings) -> Self {
        let current = Rc::new(Cell::new(None));
        let rows = Rc::new(RefCell::new(Vec::new()));
        let paths = gtk::StringList::new(&[]);
        let selection = gtk::SingleSelection::new(Some(paths.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let (header, buttons) = header_row(settings);
        let headings = Rc::new(Headings { buttons, sorted: Cell::new(None) });
        for (column, button, _) in &headings.buttons {
            let column = *column;
            let headings = headings.clone();
            button.connect_clicked(move |button| {
                let down = match headings.sorted.get() {
                    Some((sorted, down)) if sorted == column => !down,
                    _ => false,
                };
                headings.sorted.set(Some((column, down)));
                headings.show();
                let _ = button.activate_action("win.playlist-sort", Some(&(column.name(), down).to_variant()));
            });
        }

        let factory = row_factory(settings, current.clone(), rows.clone(), headings.clone());
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.add_css_class("navigation-sidebar");
        view.connect_activate(|view, position| {
            let _ = view.activate_action("win.playlist-play-index", Some(&position.to_variant()));
        });

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&view));

        let empty = gtk::Label::new(Some("Nothing yet. Right-click an item to add it, or drag files here."));
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        empty.set_margin_top(6);
        empty.set_margin_bottom(6);
        empty.set_margin_start(6);
        empty.add_css_class("caption");
        empty.add_css_class("dimmed");

        let title = gtk::Label::new(None);
        title.set_hexpand(true);
        title.set_xalign(0.0);
        title.add_css_class("caption-heading");
        title.add_css_class("dimmed");
        let detach = gtk::Button::from_icon_name("window-new-symbolic");
        detach.set_action_name(Some("win.playlist-detach"));
        detach.add_css_class("flat");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        heading.set_margin_start(8);
        heading.set_margin_end(2);
        heading.append(&title);
        heading.append(&detach);

        let (toolbar, shuffle) = toolbar(&selection);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_width_request(PANEL_WIDTH);
        content.append(&heading);
        content.append(&empty);
        content.append(&header);
        content.append(&scroller);
        content.append(&toolbar);

        // The menu's column toggles are actions of the panel, named by their settings.
        let actions = gio::SimpleActionGroup::new();
        for (key, _) in COLUMNS {
            actions.add_action(&settings.create_action(key));
        }
        content.insert_action_group("playlist", Some(&actions));

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(|target, value, _, _| {
            let dropped: Vec<String> = drag::dropped_paths(value).iter().map(|path| path.to_string_lossy().into_owned()).collect();
            let Some(widget) = target.widget() else { return false };
            !dropped.is_empty() && widget.activate_action("win.playlist-add-paths", Some(&dropped.to_variant())).is_ok()
        });
        content.add_controller(drop);

        let panel = Self {
            content,
            title,
            detach,
            shuffle,
            header,
            view,
            empty,
            scroller,
            paths,
            shown: RefCell::new(Vec::new()),
            current,
            rows,
        };
        panel.refresh();
        panel
    }

    /// The panel's widget, which the window puts beside the browse view or in a window of its own.
    pub fn content(&self) -> &gtk::Box {
        &self.content
    }

    /// Says whether the panel is in a window of its own, which the detach button then offers to undo.
    pub fn set_detached(&self, detached: bool) {
        self.detach.set_tooltip_text(Some(if detached { "Put the playlist back beside the browse view" } else { "Show the playlist in its own window" }));
    }

    /// Shows the playlist's items as they are now, in the order they are shown, with their titles and lengths. Only the
    /// items that changed are added and removed, so the list keeps its place.
    pub fn refresh(&self) {
        let (items, shuffled) = playlist::with(|list| {
            let items: Vec<String> = list.order().iter().map(|path| path.to_string_lossy().into_owned()).collect();
            (items, list.is_shuffled())
        });
        let mut shown = self.shown.borrow_mut();
        let start = items.iter().zip(shown.iter()).take_while(|(item, row)| item == row).count();
        let most = items.len().min(shown.len()) - start;
        let end = items.iter().rev().zip(shown.iter().rev()).take(most).take_while(|(item, row)| item == row).count();
        let added: Vec<&str> = items[start..items.len() - end].iter().map(String::as_str).collect();
        self.paths.splice(start as u32, (shown.len() - start - end) as u32, &added);
        // The rows are bound again, as their titles may have changed.
        self.paths.items_changed(0, items.len() as u32, items.len() as u32);

        self.title.set_label(&if items.is_empty() { "Playlist".to_owned() } else { format!("Playlist ({})", items.len()) });
        self.empty.set_visible(items.is_empty());
        self.header.set_visible(!items.is_empty());
        self.scroller.set_visible(!items.is_empty());
        if shuffled {
            self.shuffle.add_css_class("suggested-action");
        } else {
            self.shuffle.remove_css_class("suggested-action");
        }
        *shown = items;
        drop(shown);
        self.refresh_current();
    }

    /// Marks the playlist's current item, and brings it into view.
    pub fn refresh_current(&self) {
        let current = playlist::with(|list| list.current()).map(|index| index as u32);
        let moved = self.current.replace(current) != current;
        for item in self.rows.borrow().iter() {
            if let Some(row) = item.child().and_downcast::<gtk::Box>() {
                if let Some(number) = cells(&row).first() {
                    number.set_label(&number_text(item.position(), current == Some(item.position())));
                }
            }
        }
        if let Some(current) = current.filter(|_| moved) {
            self.view.scroll_to(current, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// The number of rows the panel has.
    #[cfg(test)]
    pub fn row_count(&self) -> u32 {
        self.paths.n_items()
    }
}

/// The number cell's text: the item's place, or the marker for the one shown.
fn number_text(position: u32, shown: bool) -> String {
    if shown { CURRENT_MARK.to_owned() } else { (position + 1).to_string() }
}

/// The length a time of `duration` seconds is shown as, such as `3:05` or `1:02:09`, or nothing if it is not known.
fn length_text(duration: Option<f64>) -> String {
    let Some(seconds) = duration.filter(|seconds| seconds.is_finite() && *seconds >= 0.0) else { return String::new() };
    let total = seconds.round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

/// The modification time and size of the file at `path`, which its metadata is kept against.
pub fn stamp(path: &Path) -> Option<(i64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((mtime, metadata.len()))
}

/// Shows the length of the item at `path` in `label`: from its metadata if that is known, else once it has been read.
/// The label remembers the path it shows, so a reading that arrives after the row shows another item is dropped.
fn show_length(label: &gtk::Label, path: &Path) {
    label.set_widget_name(&path.to_string_lossy());
    label.set_label("");
    let Some((mtime, size)) = stamp(path) else { return };
    let info = MediaInfo::shared();
    let shows = Rc::new({
        let weak = label.downgrade();
        let name = path.to_string_lossy().into_owned();
        move || weak.upgrade().is_some_and(|label| label.widget_name().as_str() == name)
    });
    let ready = {
        let shows = shows.clone();
        let weak = label.downgrade();
        let info = info.clone();
        let path = path.to_path_buf();
        move || {
            if let (true, Some(label)) = (shows(), weak.upgrade()) {
                label.set_label(&length_text(info.get(&path, mtime, size).and_then(|metadata| metadata.duration)));
            }
        }
    };
    let metadata = info.request(path, mtime, size, move || shows(), ready);
    label.set_label(&length_text(metadata.and_then(|metadata| metadata.duration)));
}

/// A label for a cell of a row, `width` wide if it is not -1, aligned by `xalign`.
fn cell(width: i32, xalign: f32, ellipsize: EllipsizeMode) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_width_request(width);
    label.set_xalign(xalign);
    label.set_margin_start(6);
    label.set_margin_end(6);
    label.set_ellipsize(ellipsize);
    label
}

/// The labels of a row's cells, in the order the columns are in.
fn cells(row: &gtk::Box) -> Vec<gtk::Label> {
    let mut labels = Vec::new();
    let mut child = row.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        labels.extend(widget.downcast::<gtk::Label>().ok());
    }
    labels
}

/// The row of headings above the list. Each heading is as wide as its column in the rows.
fn header_row(settings: &gio::Settings) -> (gtk::Box, Vec<(SortColumn, gtk::Button, gtk::Label)>) {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.add_css_class("caption-heading");
    header.add_css_class("dimmed");
    let position = cell(POSITION_WIDTH, 0.5, EllipsizeMode::None);
    position.set_label("#");
    settings.bind(COLUMNS[0].0, &position, "visible").build();
    header.append(&position);

    let mut buttons = Vec::new();
    for (column, key) in [(SortColumn::Name, COLUMNS[1].0), (SortColumn::Length, COLUMNS[2].0), (SortColumn::Filename, COLUMNS[3].0)] {
        let label = gtk::Label::new(None);
        label.set_xalign(if column == SortColumn::Length { 1.0 } else { 0.0 });
        label.set_margin_start(6);
        label.set_margin_end(6);
        let button = gtk::Button::new();
        button.set_child(Some(&label));
        button.add_css_class("flat");
        if column == SortColumn::Length {
            button.set_width_request(LENGTH_WIDTH);
        } else {
            button.set_hexpand(true);
        }
        settings.bind(key, &button, "visible").build();
        header.append(&button);
        buttons.push((column, button, label));
    }
    (header, buttons)
}

/// The factory of the rows. A row holds the cells of the columns, each shown or hidden by its setting, and can be
/// dragged to move its item, take items dropped on it, and open its menu on a right click.
fn row_factory(
    settings: &gio::Settings,
    current: Rc<Cell<Option<u32>>>,
    rows: Rc<RefCell<Vec<gtk::ListItem>>>,
    headings: Rc<Headings>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let setup_settings = settings.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let cells = [
            cell(POSITION_WIDTH, 0.5, EllipsizeMode::None),
            cell(-1, 0.0, EllipsizeMode::End),
            cell(LENGTH_WIDTH, 1.0, EllipsizeMode::None),
            cell(-1, 0.0, EllipsizeMode::Middle),
        ];
        for (label, (key, _)) in cells.iter().zip(COLUMNS) {
            setup_settings.bind(key, label, "visible").build();
            if label.width_request() == -1 {
                label.set_hexpand(true);
            }
            row.append(label);
        }

        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let weak = item.downgrade();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&weak.upgrade()?.position().to_value())));
        row.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = item.downgrade();
        let headings = headings.clone();
        target.connect_drop(move |target, value, _, _| {
            let (Some(item), Some(widget), Ok(from)) = (weak.upgrade(), target.widget(), value.get::<u32>()) else { return false };
            // Moving items changes the order by hand, so the list is no longer sorted by any column.
            headings.sorted.set(None);
            headings.show();
            widget.activate_action("win.playlist-move", Some(&(from, item.position()).to_variant())).is_ok()
        });
        row.add_controller(target);

        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let weak = item.downgrade();
        let menu_settings = setup_settings.clone();
        click.connect_pressed(move |gesture, _, x, y| {
            let (Some(item), Some(widget)) = (weak.upgrade(), gesture.widget()) else { return };
            let Some(path) = item.item().and_downcast::<gtk::StringObject>() else { return };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let menu = row_menu(&menu_settings, item.position(), &path.string());
            popup_menu(&widget, &menu, x, y);
        });
        row.add_controller(click);

        item.set_child(Some(&row));
    });

    let bound_rows = rows.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(row), Some(string)) = (item.child().and_downcast::<gtk::Box>(), item.item().and_downcast::<gtk::StringObject>()) else {
            return;
        };
        let text = string.string();
        let path = Path::new(text.as_str());
        let cells = cells(&row);
        let [number, name, length, filename] = cells.as_slice() else { return };
        number.set_label(&number_text(item.position(), current.get() == Some(item.position())));
        name.set_label(&playlist::with(|list| list.title(path)));
        name.set_tooltip_text(Some(path.to_string_lossy().as_ref()));
        filename.set_label(&path.to_string_lossy());
        show_length(length, path);
        let mut bound = bound_rows.borrow_mut();
        if !bound.contains(item) {
            bound.push(item.clone());
        }
    });
    factory.connect_unbind(move |_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            rows.borrow_mut().retain(|row| row != item);
        }
    });
    factory
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
    file.append_item(&item("Reveal in browser", "win.playlist-reveal", path.to_variant()));
    let open_with = gio::Menu::new();
    for script in scripts::read_scripts(settings) {
        open_with.append_item(&item(&script.name, "win.playlist-open-with", (script.name.clone(), path.to_owned()).to_variant()));
    }
    file.append_submenu(Some("Open with"), &open_with);
    menu.append_section(None, &file);

    let columns = gio::Menu::new();
    for (key, label) in COLUMNS {
        columns.append(Some(label), Some(&format!("playlist.{key}")));
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
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

/// The toolbar under the list: the buttons that act on the selected item, move through the playlist, and load, save
/// and clear its file. The shuffle button is returned, so it can be marked while the playlist is shuffled.
fn toolbar(selection: &gtk::SingleSelection) -> (gtk::Box, gtk::Button) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.set_margin_start(4);
    bar.set_margin_end(4);
    let button = |icon: &str, tooltip: &str| {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tooltip));
        button.add_css_class("flat");
        bar.append(&button);
        button
    };
    let action = |icon: &str, tooltip: &str, name: &str| {
        let button = button(icon, tooltip);
        button.set_action_name(Some(name));
        button
    };

    action("document-open-symbolic", "Load a playlist file", "win.playlist-load");
    let remove = button("list-remove-symbolic", "Remove the selected item");
    let selection_for_remove = selection.clone();
    remove.connect_clicked(move |button| {
        if let Some(index) = selected(&selection_for_remove) {
            let _ = button.activate_action("win.playlist-remove-index", Some(&index.to_variant()));
        }
    });
    let play = button("media-playback-start-symbolic", "Play the selected item, or the playlist from where it was left");
    let selection_for_play = selection.clone();
    play.connect_clicked(move |button| match selected(&selection_for_play) {
        Some(index) => {
            let _ = button.activate_action("win.playlist-play-index", Some(&index.to_variant()));
        }
        None => {
            let _ = button.activate_action("win.playlist-play", None);
        }
    });
    action("media-skip-backward-symbolic", "Previous item", "win.playlist-previous");
    action("media-skip-forward-symbolic", "Next item", "win.playlist-next");
    let shuffle = action("media-playlist-shuffle-symbolic", "Shuffle the playlist, keeping the order it was made in", "win.playlist-shuffle");
    let up = button("go-up-symbolic", "Move the selected item up");
    let selection_for_up = selection.clone();
    up.connect_clicked(move |button| {
        if let Some(from) = selected(&selection_for_up).filter(|from| *from > 0) {
            let _ = button.activate_action("win.playlist-move", Some(&(from, from - 1).to_variant()));
        }
    });
    let down = button("go-down-symbolic", "Move the selected item down");
    let selection_for_down = selection.clone();
    down.connect_clicked(move |button| {
        if let Some(from) = selected(&selection_for_down).filter(|from| *from + 1 < selection_for_down.n_items()) {
            let _ = button.activate_action("win.playlist-move", Some(&(from, from + 1).to_variant()));
        }
    });
    action("document-save-as-symbolic", "Save as a playlist file", "win.playlist-save");
    action("edit-clear-all-symbolic", "Clear the playlist", "win.playlist-clear");
    (bar, shuffle)
}

/// The position of the selected item, if one is selected.
fn selected(selection: &gtk::SingleSelection) -> Option<u32> {
    let position = selection.selected();
    (position < selection.n_items()).then_some(position)
}
