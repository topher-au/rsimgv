//! The playlist panel: buttons to play, shuffle, load, save and clear it, and its items, which can be dragged into another
//! order. Files dragged onto it are added. The panel is one widget, which sits beside the browse view or, when detached,
//! in a window of its own. What the panel asks for, it asks of the window through its `win.playlist-*` actions.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;

use super::drag;
use crate::playlist;

pub struct PlaylistPanel {
    content: gtk::Box,
    /// The heading, which says how many items there are.
    title: gtk::Label,
    detach: gtk::Button,
    shuffle: gtk::Button,
    view: gtk::ListView,
    empty: gtk::Label,
    scroller: gtk::ScrolledWindow,
    /// The items' paths, in the order shown, which the rows are made from.
    paths: gtk::StringList,
    /// What `paths` holds, to compare with the playlist.
    shown: RefCell<Vec<String>>,
    /// The item marked as the current one.
    current: Rc<Cell<Option<u32>>>,
    /// The rows showing an item now.
    rows: Rc<RefCell<Vec<gtk::ListItem>>>,
}

impl PlaylistPanel {
    /// Makes the panel, showing the playlist as it is.
    pub fn new() -> Self {
        let current = Rc::new(Cell::new(None));
        let rows = Rc::new(RefCell::new(Vec::new()));
        let paths = gtk::StringList::new(&[]);
        let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(paths.clone()))), Some(row_factory(current.clone(), rows.clone())));
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

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_width_request(PANEL_WIDTH);
        content.append(&heading);
        let (transport, shuffle) = transport_row();
        content.append(&transport);
        content.append(&file_row());
        content.append(&empty);
        content.append(&scroller);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(|target, value, _, _| {
            let dropped: Vec<String> = drag::dropped_paths(value).iter().map(|path| path.to_string_lossy().into_owned()).collect();
            let Some(widget) = target.widget() else { return false };
            !dropped.is_empty() && widget.activate_action("win.playlist-add-paths", Some(&dropped.to_variant())).is_ok()
        });
        content.add_controller(drop);

        let panel = Self { content, title, detach, shuffle, view, empty, scroller, paths, shown: RefCell::new(Vec::new()), current, rows };
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
        for row in self.rows.borrow().iter() {
            mark_current(row, current);
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

/// How wide the panel is, in pixels.
const PANEL_WIDTH: i32 = 280;

/// The buttons that play the playlist and move through it, with the one that shuffles it. The shuffle button is marked
/// while the playlist is shuffled.
fn transport_row() -> (gtk::Box, gtk::Button) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let button = |icon: &str, tooltip: &str, action: &str| {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tooltip));
        button.set_action_name(Some(action));
        button.add_css_class("flat");
        button
    };
    bar.append(&button("media-skip-backward-symbolic", "Previous item", "win.playlist-previous"));
    bar.append(&button("media-playback-start-symbolic", "Play the playlist", "win.playlist-play"));
    bar.append(&button("media-skip-forward-symbolic", "Next item", "win.playlist-next"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    let shuffle = button("media-playlist-shuffle-symbolic", "Shuffle the playlist, keeping the order it was made in", "win.playlist-shuffle");
    bar.append(&shuffle);
    (bar, shuffle)
}

/// The buttons that load, save and clear the playlist's files.
fn file_row() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let button = |icon: &str, tooltip: &str, action: &str| {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tooltip));
        button.set_action_name(Some(action));
        button.add_css_class("flat");
        button
    };
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    bar.append(&button("document-open-symbolic", "Load a playlist file", "win.playlist-load"));
    bar.append(&button("document-save-as-symbolic", "Save as a playlist file", "win.playlist-save"));
    bar.append(&button("edit-clear-all-symbolic", "Clear the playlist", "win.playlist-clear"));
    bar
}

/// Makes each row a marker for the current item, the file's name, and a button to remove it. A row can be dragged onto
/// another to take its place, and is activated to play from it.
fn row_factory(current: Rc<Cell<Option<u32>>>, rows: Rc<RefCell<Vec<gtk::ListItem>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&gtk::Image::from_icon_name("media-playback-start-symbolic"));
        let name = gtk::Label::new(None);
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&name);

        let remove = gtk::Button::from_icon_name("list-remove-symbolic");
        remove.set_tooltip_text(Some("Remove from the playlist"));
        remove.add_css_class("flat");
        let weak = item.downgrade();
        remove.connect_clicked(move |button| {
            if let Some(item) = weak.upgrade() {
                let _ = button.activate_action("win.playlist-remove-index", Some(&item.position().to_variant()));
            }
        });
        row.append(&remove);

        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let weak = item.downgrade();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&weak.upgrade()?.position().to_value())));
        row.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = item.downgrade();
        target.connect_drop(move |target, value, _, _| {
            let (Some(item), Some(widget), Ok(from)) = (weak.upgrade(), target.widget(), value.get::<u32>()) else { return false };
            widget.activate_action("win.playlist-move", Some(&(from, item.position()).to_variant())).is_ok()
        });
        row.add_controller(target);

        item.set_child(Some(&row));
    });
    let bound = rows.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let path = item.item().and_downcast::<gtk::StringObject>().map(|text| text.string());
        let name = item.child().and_then(|row| row.first_child()).and_then(|marker| marker.next_sibling()).and_downcast::<gtk::Label>();
        if let (Some(path), Some(name)) = (path, name) {
            name.set_label(&Path::new(path.as_str()).file_name().map_or_else(|| path.to_string(), |name| name.to_string_lossy().into_owned()));
            name.set_tooltip_text(Some(path.as_str()));
        }
        mark_current(item, current.get());
        bound.borrow_mut().push(item.clone());
    });
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        rows.borrow_mut().retain(|row| row != item);
    });
    factory
}

/// Shows the marker on the row of `item` if it is the `current` one, keeping its place otherwise so the names line up.
fn mark_current(item: &gtk::ListItem, current: Option<u32>) {
    if let Some(marker) = item.child().and_then(|row| row.first_child()) {
        marker.set_opacity(if current == Some(item.position()) { 1.0 } else { 0.0 });
    }
}
