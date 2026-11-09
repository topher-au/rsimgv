//! The Viewing History dialog: the pictures and videos that have been viewed in a table of their file names, how many
//! times each has been viewed and how long it has been watched. A click on a column's heading sorts the table by it, and
//! activating a row opens the file in the viewer.
//!
//! The history is kept by `storage::watch_history`, and shared by every window.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk::{gio, glib};

use adw::prelude::{AdwDialogExt, AlertDialogExt};

use crate::storage::watch_history::{format_duration, WatchEntry, WatchHistory};

use super::window::{Destination, Window};

/// One column of the table: the part of a watch entry it shows and sorts by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Filename,
    Views,
    Watched,
}

impl Column {
    /// The column's heading.
    fn title(self) -> &'static str {
        match self {
            Self::Filename => "Filename",
            Self::Views => "Watch Count",
            Self::Watched => "Total Time",
        }
    }
}

/// Shows the viewing history over `parent`.
pub fn present(parent: &Window, history: &'static WatchHistory) {
    let entries = match history.recent(500) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("rsimgv: could not read the viewing history: {err}");
            Vec::new()
        }
    };
    // The table holds each entry's content hash, so sorting and cells find the rest of the entry here. A hash is the
    // history's key, so no entry is lost.
    let by_hash: Rc<HashMap<String, WatchEntry>> =
        Rc::new(entries.iter().map(|entry| (entry.hash.clone(), entry.clone())).collect());
    let store = gio::ListStore::new::<gtk::StringObject>();
    for entry in &entries {
        store.append(&gtk::StringObject::new(&entry.hash));
    }

    let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
    let selection = gtk::SingleSelection::new(Some(sorted.clone()));
    let view = gtk::ColumnView::new(Some(selection));
    // The view sorts by the column headings, so its sorter is what sorts the rows. An entry is newest first until a
    // heading is clicked, as the history comes in that order.
    if let Some(sorter) = view.sorter() {
        sorted.set_sorter(Some(&sorter));
    }
    view.add_css_class("data-table");
    view.set_single_click_activate(true);
    for column in [Column::Filename, Column::Views, Column::Watched] {
        let factory = cell_factory(column, by_hash.clone());
        let table_column = gtk::ColumnViewColumn::new(Some(column.title()), Some(factory));
        table_column.set_resizable(true);
        table_column.set_expand(column == Column::Filename);
        table_column.set_sorter(Some(&column_sorter(column, by_hash.clone())));
        view.append_column(&table_column);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&view)
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("No Viewing History")
        .description("Pictures and videos you view will be listed here.")
        .vexpand(true)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&scrolled, Some("table"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "table" });

    let clear = gtk::Button::from_icon_name("user-trash-symbolic");
    clear.set_tooltip_text(Some("Clear Viewing History"));
    clear.set_sensitive(!entries.is_empty());
    clear.add_css_class("flat");
    let close = gtk::Button::with_label("Close");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Viewing History", "")));
    header.pack_start(&clear);
    header.pack_end(&close);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let dialog = adw::Dialog::builder().content_width(680).content_height(560).child(&toolbar).build();
    view.connect_activate({
        let weak = parent.downgrade();
        let entries = by_hash.clone();
        let dialog = dialog.clone();
        move |view, position| {
            let Some(hash) = view.model().and_then(|model| model.item(position)).and_downcast::<gtk::StringObject>() else {
                return;
            };
            let Some(entry) = entries.get(hash.string().as_str()) else { return };
            if !entry.path.is_file() {
                return;
            }
            if let Some(window) = weak.upgrade() {
                window.open_path(&entry.path, Destination::CurrentTab, false);
                dialog.close();
            }
        }
    });
    close.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    clear.connect_clicked({
        let stack = stack.clone();
        let store = store.clone();
        let clear = clear.clone();
        move |button| {
            let confirm = adw::AlertDialog::new(
                Some("Clear the Viewing History?"),
                Some("How many times and how long each picture and video has been watched will be forgotten."),
            );
            confirm.add_response("cancel", "Cancel");
            confirm.add_response("clear", "Clear");
            confirm.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.connect_response(Some("clear"), {
                let stack = stack.clone();
                let store = store.clone();
                let clear = clear.clone();
                move |confirm, _| {
                    if let Err(err) = history.clear() {
                        eprintln!("rsimgv: could not clear the viewing history: {err}");
                        return;
                    }
                    confirm.close();
                    store.remove_all();
                    stack.set_visible_child_name("empty");
                    clear.set_sensitive(false);
                }
            });
            confirm.present(Some(button));
        }
    });

    dialog.present(Some(parent));
}

/// One column's cells, which are labels showing that part of the entry.
fn cell_factory(column: Column, entries: Rc<HashMap<String, WatchEntry>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::new(None);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_xalign(if column == Column::Filename { 0.0 } else { 1.0 });
        label.set_ellipsize(if column == Column::Filename { EllipsizeMode::Middle } else { EllipsizeMode::End });
        item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(label), Some(string)) =
            (item.child().and_downcast::<gtk::Label>(), item.item().and_downcast::<gtk::StringObject>())
        else {
            return;
        };
        let Some(entry) = entries.get(string.string().as_str()) else { return };
        match column {
            Column::Filename => {
                label.set_text(&filename(entry));
                label.set_tooltip_text(Some(&tooltip(entry)));
                if entry.path.is_file() {
                    label.remove_css_class("dim-label");
                } else {
                    label.add_css_class("dim-label");
                }
            }
            Column::Views => label.set_text(&entry.views.to_string()),
            Column::Watched => label.set_text(&format_duration(entry.watched_seconds)),
        }
    });
    factory
}

/// The sorter of one column: it compares entries by that column's value.
fn column_sorter(column: Column, entries: Rc<HashMap<String, WatchEntry>>) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |left, right| {
        let (Some(left), Some(right)) = (entry_for(&entries, left), entry_for(&entries, right)) else {
            return gtk::Ordering::Equal;
        };
        let order = match column {
            // Names sort without regard to case, and files sharing a name sort by their folder.
            Column::Filename => filename(left)
                .to_lowercase()
                .cmp(&filename(right).to_lowercase())
                .then_with(|| left.path.cmp(&right.path)),
            Column::Views => left.views.cmp(&right.views),
            Column::Watched => left.watched_seconds.total_cmp(&right.watched_seconds),
        };
        match order {
            Ordering::Less => gtk::Ordering::Smaller,
            Ordering::Equal => gtk::Ordering::Equal,
            Ordering::Greater => gtk::Ordering::Larger,
        }
    })
}

/// The entry a row stands for, from the content hash the table holds.
fn entry_for<'a>(entries: &'a HashMap<String, WatchEntry>, object: &glib::Object) -> Option<&'a WatchEntry> {
    let hash = object.downcast_ref::<gtk::StringObject>()?;
    entries.get(hash.string().as_str())
}

/// The entry's file name, or its whole path when it has none.
fn filename(entry: &WatchEntry) -> String {
    entry.path.file_name().map_or_else(|| entry.path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// The entry's path, and when it was last viewed, or that it is no longer there.
fn tooltip(entry: &WatchEntry) -> String {
    if !entry.path.is_file() {
        return format!("{} is no longer there", entry.path.display());
    }
    let last = glib::DateTime::from_unix_local(entry.last_viewed)
        .ok()
        .and_then(|time| time.format("%Y-%m-%d %H:%M").ok())
        .map_or_else(String::new, |time| format!("\nLast viewed {time}"));
    format!("{}{last}", entry.path.display())
}
