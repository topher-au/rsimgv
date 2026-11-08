//! The Viewing History dialog: the pictures and videos that have been viewed, newest first, with how many times each
//! has been viewed and how long it has been watched. Activating a row opens the file in the viewer.
//!
//! The history is kept by `storage::watch_history`, and shared by every window.

use gtk::prelude::*;
use gtk::glib;

use adw::prelude::{ActionRowExt, AdwDialogExt, AlertDialogExt};

use crate::storage::watch_history::{format_duration, WatchEntry, WatchHistory};

use super::window::{Destination, Window};

/// Shows the viewing history over `parent`.
pub fn present(parent: &Window, history: &'static WatchHistory) {
    let entries = match history.recent(500) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("rsimgv: could not read the viewing history: {err}");
            Vec::new()
        }
    };
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    for entry in &entries {
        list.append(&history_row(entry));
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("No Viewing History")
        .description("Pictures and videos you view will be listed here.")
        .vexpand(true)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "list" });

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
    let dialog = adw::Dialog::builder().content_width(640).content_height(560).child(&toolbar).build();
    list.connect_row_activated({
        let weak = parent.downgrade();
        let entries = entries.clone();
        let dialog = dialog.clone();
        move |_, row| {
            let Some(entry) = entries.get(row.index() as usize) else { return };
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
        let list = list.clone();
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
                let list = list.clone();
                let clear = clear.clone();
                move |confirm, _| {
                    if let Err(err) = history.clear() {
                        eprintln!("rsimgv: could not clear the viewing history: {err}");
                        return;
                    }
                    confirm.close();
                    while let Some(child) = list.first_child() {
                        list.remove(&child);
                    }
                    stack.set_visible_child_name("empty");
                    clear.set_sensitive(false);
                }
            });
            confirm.present(Some(button));
        }
    });

    dialog.present(Some(parent));
}

/// One row of the history: the file's name, then its views, watched time, when it was last viewed and its folder.
fn history_row(entry: &WatchEntry) -> adw::ActionRow {
    let name = entry
        .path
        .file_name()
        .map_or_else(|| entry.path.display().to_string(), |name| name.to_string_lossy().into_owned());
    let folder = entry.path.parent().map_or_else(String::new, |folder| folder.display().to_string());
    let views = format!("{} view{}", entry.views, if entry.views == 1 { "" } else { "s" });
    let watched = format!("{} watched", format_duration(entry.watched_seconds));
    let last = glib::DateTime::from_unix_local(entry.last_viewed)
        .ok()
        .and_then(|time| time.format("%Y-%m-%d %H:%M").ok())
        .map_or_else(String::new, |time| format!("last viewed {time}"));
    let mut details = vec![views, watched];
    if !last.is_empty() {
        details.push(last);
    }
    let row = adw::ActionRow::builder()
        .title(name)
        .subtitle(format!("{}\n{folder}", details.join(" · ")))
        .subtitle_lines(2)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
    row.set_tooltip_text(Some(entry.path.to_string_lossy().as_ref()));
    if !entry.path.is_file() {
        row.set_activatable(false);
        row.set_tooltip_text(Some(format!("{} is no longer there", entry.path.display()).as_str()));
    }
    row
}
