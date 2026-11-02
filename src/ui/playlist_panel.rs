//! The playlist panel: a heading with the detach button, the items in columns (their number, name, length and path), and a
//! toolbar of buttons that act on the list. The panel is one widget, which sits beside the browse view or, when detached,
//! in a window of its own. What the panel asks for, it asks of the window through its `win.playlist-*` actions.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;

use super::drag;
use crate::playlist;

/// How wide the panel is, in pixels.
const PANEL_WIDTH: i32 = 400;

/// The symbol that marks the item shown, in the number column.
const CURRENT_MARK: &str = "▶";

pub struct PlaylistPanel {
    content: gtk::Box,
    /// The heading, which says how many items there are.
    title: gtk::Label,
    detach: gtk::Button,
    shuffle: gtk::Button,
    view: gtk::ColumnView,
    empty: gtk::Label,
    scroller: gtk::ScrolledWindow,
    /// The items' paths, in the order shown.
    paths: gtk::StringList,
    /// What `paths` holds, to compare with the playlist.
    shown: RefCell<Vec<String>>,
    /// The item marked as the current one.
    current: Rc<Cell<Option<u32>>>,
    /// The number cells showing an item now, which show the marker on the current one.
    numbers: Rc<RefCell<Vec<gtk::ListItem>>>,
}

impl PlaylistPanel {
    /// Makes the panel, showing the playlist as it is.
    pub fn new() -> Self {
        let current = Rc::new(Cell::new(None));
        let numbers = Rc::new(RefCell::new(Vec::new()));
        let paths = gtk::StringList::new(&[]);
        let selection = gtk::SingleSelection::new(Some(paths.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("data-table");
        view.connect_activate(|view, position| {
            let _ = view.activate_action("win.playlist-play-index", Some(&position.to_variant()));
        });
        view.append_column(&column("", number_factory(current.clone(), numbers.clone()), None));
        view.append_column(&column("Name", text_factory(file_name), Some(true)));
        view.append_column(&column("Length", text_factory(|_| String::new()), None));
        view.append_column(&column("Filename / URL", text_factory(|path| path.display().to_string()), Some(true)));

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Automatic);
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
        content.append(&scroller);
        content.append(&toolbar);

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
            view,
            empty,
            scroller,
            paths,
            shown: RefCell::new(Vec::new()),
            current,
            numbers,
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

    /// Shows the playlist's items as they are now, in the order they are shown. Only the items that changed are touched,
    /// so the list keeps its place.
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

        self.title.set_label(&if items.is_empty() { "Playlist".to_owned() } else { format!("Playlist ({})", items.len()) });
        self.empty.set_visible(items.is_empty());
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
        for item in self.numbers.borrow().iter() {
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_label(&number_text(item.position(), current == Some(item.position())));
            }
        }
        if let Some(current) = current.filter(|_| moved) {
            self.view.scroll_to(current, None, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// The number of rows the panel has.
    #[cfg(test)]
    pub fn row_count(&self) -> u32 {
        self.paths.n_items()
    }
}

/// The text of a number cell: the item's place, or the marker for the one shown.
fn number_text(position: u32, shown: bool) -> String {
    if shown { CURRENT_MARK.to_owned() } else { (position + 1).to_string() }
}

/// The file's name, for the name column.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// A column of the list, with its title. `expand` makes it take the spare width, and columns that do not expand are as
/// wide as their text.
fn column(title: &str, factory: gtk::SignalListItemFactory, expand: Option<bool>) -> gtk::ColumnViewColumn {
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_resizable(true);
    if let Some(expand) = expand {
        column.set_expand(expand);
    }
    column
}

/// A cell that shows the text `text` makes of the item's path.
fn text_factory(text: fn(&Path) -> String) -> gtk::SignalListItemFactory {
    let factory = label_factory();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(label), Some(path)) = (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>()) else {
            return;
        };
        label.set_label(&text(Path::new(path.string().as_str())));
        label.set_tooltip_text(Some(path.string().as_str()));
    });
    factory
}

/// The number cell, which shows the marker on the item shown. Its cells are kept, so the marker can move without a
/// rebind.
fn number_factory(current: Rc<Cell<Option<u32>>>, numbers: Rc<RefCell<Vec<gtk::ListItem>>>) -> gtk::SignalListItemFactory {
    let factory = label_factory();
    let bound = numbers.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
        label.set_label(&number_text(item.position(), current.get() == Some(item.position())));
        bound.borrow_mut().push(item.clone());
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        numbers.borrow_mut().retain(|cell| cell != item);
    });
    factory
}

/// The cell every column uses: a label, which can be dragged to move its item and takes items dropped on it.
fn label_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);

        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let weak = item.downgrade();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&weak.upgrade()?.position().to_value())));
        label.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = item.downgrade();
        target.connect_drop(move |target, value, _, _| {
            let (Some(item), Some(widget), Ok(from)) = (weak.upgrade(), target.widget(), value.get::<u32>()) else { return false };
            widget.activate_action("win.playlist-move", Some(&(from, item.position()).to_variant())).is_ok()
        });
        label.add_controller(target);

        item.set_child(Some(&label));
    });
    factory
}

/// The position of the selected item, if one is selected.
fn selected(selection: &gtk::SingleSelection) -> Option<u32> {
    let position = selection.selected();
    (position < selection.n_items()).then_some(position)
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
