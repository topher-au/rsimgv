//! The search bar of the browse view: under the header, it slides down with a field that filters the folder's items by
//! their name or an attribute, and a switch that searches the folders under it too. Shown and hidden by the header's
//! search button, or the browse view's toggle-search action.

use std::rc::Rc;

use gtk::prelude::*;

use super::browse_view::{Search, SearchField};

#[derive(Clone)]
pub struct BrowseSearch {
    bar: gtk::SearchBar,
    entry: gtk::SearchEntry,
    field: gtk::DropDown,
    subfolders: gtk::ToggleButton,
}

impl BrowseSearch {
    /// Builds the contents of `bar`. `changed` gets the search whenever its text, field or subfolders switch changes,
    /// and when the bar closes, which clears the text.
    pub fn new(bar: &gtk::SearchBar, changed: impl Fn(Search) + 'static) -> Self {
        let titles: Vec<&str> = SearchField::ALL.iter().map(|field| field.title()).collect();
        let field = gtk::DropDown::from_strings(&titles);
        field.set_tooltip_text(Some("What the text is matched against"));

        let entry = gtk::SearchEntry::new();
        entry.set_hexpand(true);
        entry.set_placeholder_text(Some("Filter this folder"));

        let subfolders = gtk::ToggleButton::with_label("Subfolders");
        subfolders.set_tooltip_text(Some("Also search the folders under this one"));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_margin_start(6);
        row.set_margin_end(6);
        row.append(&field);
        row.append(&entry);
        row.append(&subfolders);
        bar.set_show_close_button(true);
        bar.set_child(Some(&row));
        bar.connect_entry(&entry);

        let search = Self { bar: bar.clone(), entry, field, subfolders };
        let report = {
            let search = search.clone();
            Rc::new(move || changed(search.search()))
        };
        search.entry.connect_search_changed({
            let report = report.clone();
            move |_| report()
        });
        search.field.connect_selected_notify({
            let report = report.clone();
            move |_| report()
        });
        search.subfolders.connect_toggled({
            let (report, entry) = (report.clone(), search.entry.clone());
            move |button| {
                let placeholder = if button.is_active() { "Search subfolders" } else { "Filter this folder" };
                entry.set_placeholder_text(Some(placeholder));
                report()
            }
        });
        bar.connect_search_mode_enabled_notify({
            let entry = search.entry.clone();
            move |bar| {
                if bar.is_search_mode() {
                    entry.grab_focus();
                } else {
                    // Closing the bar shows every item again.
                    entry.set_text("");
                }
            }
        });
        search
    }

    /// The search the bar currently makes. With the bar closed, its text is empty, so nothing is filtered.
    pub fn search(&self) -> Search {
        let field = usize::try_from(self.field.selected()).ok().and_then(|index| SearchField::ALL.get(index)).copied();
        Search::new(&self.entry.text(), field.unwrap_or_default(), self.subfolders.is_active())
    }

    /// Shows the bar, or hides it.
    pub fn toggle(&self) {
        self.bar.set_search_mode(!self.bar.is_search_mode());
    }
}
