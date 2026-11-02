//! The playlist in the sidebar: buttons to play, load, save and clear it, and its items, which can be dragged into another
//! order. Files dragged onto it are added. What the panel asks for, it asks of the window through its `win.playlist-*`
//! actions.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::gdk;

use super::drag;
use crate::playlist;

/// The tallest the list gets before it scrolls, in pixels, so the folder tree below it keeps its room.
const MAX_LIST_HEIGHT: i32 = 260;

pub struct PlaylistPanel {
    expander: gtk::Expander,
    /// The section's heading, which says how many items there are.
    title: gtk::Label,
    view: gtk::ListView,
    empty: gtk::Label,
    scroller: gtk::ScrolledWindow,
    /// The items' paths, which the rows are made from.
    paths: gtk::StringList,
    /// What `paths` holds, to compare with the playlist.
    shown: RefCell<Vec<String>>,
    /// The item marked as the current one.
    current: Rc<Cell<Option<u32>>>,
    /// The rows showing an item now.
    rows: Rc<RefCell<Vec<gtk::ListItem>>>,
}

impl PlaylistPanel {
    /// Fills `expander` with the panel, showing the playlist as it is. It starts open if there is anything to show.
    pub fn new(expander: &gtk::Expander) -> Self {
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
        scroller.set_propagate_natural_height(true);
        scroller.set_max_content_height(MAX_LIST_HEIGHT);
        scroller.set_child(Some(&view));

        let empty = gtk::Label::new(Some("Nothing yet. Right-click an item to add it, or drag files here."));
        empty.set_wrap(true);
        empty.set_xalign(0.0);
        empty.set_margin_top(6);
        empty.set_margin_bottom(6);
        empty.set_margin_start(6);
        empty.add_css_class("caption");
        empty.add_css_class("dimmed");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&toolbar());
        content.append(&empty);
        content.append(&scroller);
        expander.set_child(Some(&content));

        // The heading looks like those of the sidebar's other sections.
        let title = gtk::Label::new(None);
        title.set_hexpand(true);
        title.set_xalign(0.0);
        title.add_css_class("caption-heading");
        title.add_css_class("dimmed");
        expander.set_label_widget(Some(&title));

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(|target, value, _, _| {
            let dropped: Vec<String> = drag::dropped_paths(value).iter().map(|path| path.to_string_lossy().into_owned()).collect();
            let Some(widget) = target.widget() else { return false };
            !dropped.is_empty() && widget.activate_action("win.playlist-add-paths", Some(&dropped.to_variant())).is_ok()
        });
        expander.add_controller(drop);

        let panel = Self { expander: expander.clone(), title, view, empty, scroller, paths, shown: RefCell::new(Vec::new()), current, rows };
        panel.refresh();
        expander.set_expanded(!panel.shown.borrow().is_empty());
        panel
    }

    /// Shows the playlist's items as they are now. Only the items that changed are touched, so the list keeps its place.
    pub fn refresh(&self) {
        let items: Vec<String> = playlist::with(|list| list.items().iter().map(|path| path.to_string_lossy().into_owned()).collect());
        let mut shown = self.shown.borrow_mut();
        let start = items.iter().zip(shown.iter()).take_while(|(item, row)| item == row).count();
        let most = items.len().min(shown.len()) - start;
        let end = items.iter().rev().zip(shown.iter().rev()).take(most).take_while(|(item, row)| item == row).count();
        let added: Vec<&str> = items[start..items.len() - end].iter().map(String::as_str).collect();
        self.paths.splice(start as u32, (shown.len() - start - end) as u32, &added);

        self.title.set_label(&if items.is_empty() { "Playlist".to_owned() } else { format!("Playlist ({})", items.len()) });
        self.empty.set_visible(items.is_empty());
        self.scroller.set_visible(!items.is_empty());
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

    /// Shows the panel's items, or hides them.
    pub fn toggle_expanded(&self) {
        self.expander.set_expanded(!self.expander.is_expanded());
    }

    /// Shows the panel's items.
    pub fn expand(&self) {
        self.expander.set_expanded(true);
    }

    /// The number of rows the panel has.
    #[cfg(test)]
    pub fn row_count(&self) -> u32 {
        self.paths.n_items()
    }
}

/// The buttons above the list.
fn toolbar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let button = |icon: &str, tooltip: &str, action: &str| {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tooltip));
        button.set_action_name(Some(action));
        button.add_css_class("flat");
        button
    };
    bar.append(&button("media-playback-start-symbolic", "Play the playlist", "win.playlist-play"));
    bar.append(&button("media-skip-backward-symbolic", "Previous item", "win.playlist-previous"));
    bar.append(&button("media-skip-forward-symbolic", "Next item", "win.playlist-next"));
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
