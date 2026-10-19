//! The keybindings editor for the preferences: a searchable list of every binding with its scope and chord,
//! the buttons that add, change and remove them, a line that flags conflicts, and import, export and reset.

use std::cell::{OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::keybindings::{self, actions::ActionInfo, actions::ACTIONS};
use super::binding_dialog;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct KeybindingList {
        pub(super) list: OnceCell<gtk::ListBox>,
        pub(super) search: OnceCell<gtk::SearchEntry>,
        pub(super) conflicts: OnceCell<gtk::Label>,
        pub(super) edit: OnceCell<gtk::Button>,
        pub(super) remove: OnceCell<gtk::Button>,
        /// The action and chord of each row, in the list's order.
        pub(super) bindings: RefCell<Vec<(&'static ActionInfo, String)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for KeybindingList {
        const NAME: &'static str = "RsimgvKeybindingList";
        type Type = super::KeybindingList;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for KeybindingList {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_spacing(8);

            let add = icon_button("list-add-symbolic", "Add binding");
            let edit = icon_button("document-edit-symbolic", "Change binding");
            let remove = icon_button("list-remove-symbolic", "Remove binding");
            let search = gtk::SearchEntry::new();
            search.set_hexpand(true);
            search.set_placeholder_text(Some("Search actions or shortcuts"));
            let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            for widget in [add.upcast_ref::<gtk::Widget>(), edit.upcast_ref(), remove.upcast_ref(), search.upcast_ref()] {
                toolbar.append(widget);
            }
            obj.append(&toolbar);

            let conflicts = gtk::Label::new(None);
            conflicts.set_xalign(0.0);
            conflicts.set_wrap(true);
            conflicts.add_css_class("warning");
            conflicts.set_visible(false);
            obj.append(&conflicts);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("boxed-list");
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_child(Some(&list));
            scroller.set_vexpand(true);
            scroller.set_min_content_height(320);
            scroller.set_has_frame(true);
            obj.append(&scroller);

            let import = gtk::Button::with_label("Import");
            let export = gtk::Button::with_label("Export");
            let reset = gtk::Button::with_label("Reset to Defaults");
            reset.set_hexpand(true);
            reset.set_halign(gtk::Align::End);
            let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            footer.append(&import);
            footer.append(&export);
            footer.append(&reset);
            obj.append(&footer);

            let weak = obj.downgrade();
            add.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.add_binding())
            });
            edit.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.change_selected())
            });
            remove.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.remove_selected())
            });
            list.connect_row_activated({
                let weak = weak.clone();
                move |_, _| with_list(&weak, |list| list.change_selected())
            });
            list.connect_selected_rows_changed({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.update_buttons())
            });
            search.connect_search_changed({
                let list = list.clone();
                move |_| list.invalidate_filter()
            });
            list.set_filter_func({
                let weak = weak.clone();
                let search = search.clone();
                move |row| {
                    let needle = search.text().to_lowercase();
                    if needle.is_empty() {
                        return true;
                    }
                    let Some(obj) = weak.upgrade() else { return true };
                    let bindings = obj.imp().bindings.borrow();
                    bindings.get(row.index() as usize).is_some_and(|(action, chord)| {
                        action.label.to_lowercase().contains(&needle)
                            || action.name.contains(&needle)
                            || chord.contains(&needle)
                    })
                }
            });
            import.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.import())
            });
            export.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.export())
            });
            reset.connect_clicked(move |_| with_list(&weak, |list| list.reset()));

            let _ = self.list.set(list);
            let _ = self.search.set(search);
            let _ = self.conflicts.set(conflicts);
            let _ = self.edit.set(edit);
            let _ = self.remove.set(remove);
        }
    }

    impl WidgetImpl for KeybindingList {}
    impl BoxImpl for KeybindingList {}

    fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(tooltip));
        button
    }

    /// Runs `f` on the editor if it still exists.
    fn with_list(weak: &glib::WeakRef<super::KeybindingList>, f: impl FnOnce(&super::KeybindingList)) {
        if let Some(list) = weak.upgrade() {
            f(&list);
        }
    }
}

glib::wrapper! {
    pub struct KeybindingList(ObjectSubclass<imp::KeybindingList>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl KeybindingList {
    /// The editor, showing the bindings in the live table.
    pub fn new() -> Self {
        let editor: Self = glib::Object::new();
        editor.refresh();
        editor
    }

    /// Rebuilds the rows from the live table: one for each chord of each action, then the conflicts line.
    fn refresh(&self) {
        let imp = self.imp();
        let list = imp.list.get().expect("list is built in constructed");
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        let mut bindings = Vec::new();
        for action in ACTIONS {
            for chord in keybindings::with_table(|table| table.chords(action.name).to_vec()) {
                list.append(&binding_row(action, &chord));
                bindings.push((action, chord));
            }
        }
        imp.bindings.replace(bindings);
        list.invalidate_filter();
        self.update_conflicts();
        self.update_buttons();
    }

    /// Shows the first chord that is bound to more than one action whose scopes overlap, if there is one.
    fn update_conflicts(&self) {
        let imp = self.imp();
        let conflict = imp.bindings.borrow().iter().find_map(|(action, chord)| {
            let others = keybindings::with_table(|table| table.conflicts(action.name, chord));
            (!others.is_empty()).then(|| {
                let names: Vec<&str> = std::iter::once(action.name).chain(others.iter().map(|other| other.name)).collect();
                format!("Conflicts - {chord} in {}: {}", action.scope.title().to_lowercase(), names.join(", "))
            })
        });
        let label = imp.conflicts.get().expect("conflicts is built in constructed");
        label.set_label(conflict.as_deref().unwrap_or_default());
        label.set_visible(conflict.is_some());
    }

    fn update_buttons(&self) {
        let imp = self.imp();
        let has_selection = imp.list.get().is_some_and(|list| list.selected_row().is_some());
        imp.edit.get().expect("edit is built in constructed").set_sensitive(has_selection);
        imp.remove.get().expect("remove is built in constructed").set_sensitive(has_selection);
    }

    /// The binding on the selected row, if a row is selected.
    fn selected_binding(&self) -> Option<(&'static ActionInfo, String)> {
        let imp = self.imp();
        let row = imp.list.get()?.selected_row()?;
        imp.bindings.borrow().get(row.index() as usize).cloned()
    }

    /// Saves the bindings file and shows the new rows.
    fn commit(&self) {
        if let Err(err) = keybindings::save() {
            eprintln!("rsimgv: could not save keybindings: {err}");
        }
        self.refresh();
    }

    fn add_binding(&self) {
        let weak = self.downgrade();
        binding_dialog::show(self, None, move |action, chord| {
            keybindings::with_table(|table| table.add(action.name, &chord));
            if let Some(list) = weak.upgrade() {
                list.commit();
            }
        });
    }

    fn change_selected(&self) {
        let Some((action, chord)) = self.selected_binding() else { return };
        let weak = self.downgrade();
        let previous = chord.clone();
        binding_dialog::show(self, Some((action, chord)), move |new_action, new_chord| {
            keybindings::with_table(|table| {
                table.remove(action.name, &previous);
                table.add(new_action.name, &new_chord);
            });
            if let Some(list) = weak.upgrade() {
                list.commit();
            }
        });
    }

    fn remove_selected(&self) {
        let Some((action, chord)) = self.selected_binding() else { return };
        keybindings::with_table(|table| table.remove(action.name, &chord));
        self.commit();
    }

    fn reset(&self) {
        keybindings::with_table(|table| table.reset_all());
        self.commit();
    }

    /// Loads a bindings file over the current set. Problems are printed, and the rest still apply.
    fn import(&self) {
        let dialog = gtk::FileDialog::builder().title("Import Keybindings").build();
        let parent = self.root().and_downcast::<gtk::Window>();
        let weak = self.downgrade();
        dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    for problem in keybindings::with_table(|table| table.apply_yaml(&text)) {
                        eprintln!("rsimgv: keybindings: {problem}");
                    }
                }
                Err(err) => eprintln!("rsimgv: could not read {}: {err}", path.display()),
            }
            if let Some(list) = weak.upgrade() {
                list.commit();
            }
        });
    }

    /// Writes the bindings that differ from the defaults to a chosen file.
    fn export(&self) {
        let dialog = gtk::FileDialog::builder().title("Export Keybindings").initial_name("keybindings.yaml").build();
        let parent = self.root().and_downcast::<gtk::Window>();
        dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else { return };
            let text = keybindings::with_table(|table| table.to_yaml());
            if let Err(err) = std::fs::write(&path, text) {
                eprintln!("rsimgv: could not write {}: {err}", path.display());
            }
        });
    }
}

/// A row: the action's icon and label, its scope, and its chord in monospace.
fn binding_row(action: &'static ActionInfo, chord: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(12);
    content.set_margin_end(12);

    content.append(&gtk::Image::from_icon_name(action.icon));
    let label = gtk::Label::new(Some(action.label));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    content.append(&label);

    let scope = gtk::Label::new(Some(action.scope.title()));
    scope.add_css_class("dim-label");
    content.append(&scope);

    let key = gtk::Label::new(Some(chord));
    key.add_css_class("monospace");
    content.append(&key);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&content));
    row
}
