//! The keybindings editor for the preferences: an action picker and add button, a line that flags conflicts,
//! a list of every binding with its scope, chord and a remove button, a search box, and import, export and
//! reset. Activating a row changes its chord.

use std::cell::{OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::keybindings::{self, actions::value_info, actions::ActionInfo, actions::ACTIONS, Binding, BindingTable};
use super::binding_dialog;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct KeybindingList {
        pub(super) list: OnceCell<gtk::ListBox>,
        pub(super) search: OnceCell<gtk::SearchEntry>,
        pub(super) conflicts: OnceCell<gtk::Label>,
        pub(super) actions: OnceCell<gtk::DropDown>,
        pub(super) undo: OnceCell<gtk::Button>,
        /// The bindings as they were when the editor opened, which undo restores.
        pub(super) snapshot: RefCell<Option<BindingTable>>,
        /// The action and binding of each row, in the list's order.
        pub(super) bindings: RefCell<Vec<(&'static ActionInfo, Binding)>>,
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

            // Every action, to pick the one the add button binds.
            // The popup lists each action's name over its description; the button shows the name.
            // Each item's string holds the action's name, label and description, so the search matches any of them.
            let texts: Vec<String> =
                ACTIONS.iter().map(|action| format!("{}\n{}\n{}", action.name, action.label, action.description)).collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            let actions = gtk::DropDown::new(
                Some(gtk::StringList::new(&texts)),
                Some(gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")),
            );
            actions.set_enable_search(true);
            actions.add_css_class("action-picker");
            actions.set_search_match_mode(gtk::StringFilterMatchMode::Substring);
            actions.set_factory(Some(&action_factory(false)));
            actions.set_list_factory(Some(&action_factory(true)));
            actions.set_hexpand(true);
            let add = icon_button("list-add-symbolic", "Add binding");
            let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            toolbar.append(&actions);
            toolbar.append(&add);
            obj.append(&toolbar);

            let conflicts = gtk::Label::new(None);
            conflicts.set_xalign(0.0);
            conflicts.set_wrap(true);
            conflicts.add_css_class("warning");
            conflicts.set_visible(false);
            obj.append(&conflicts);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_child(Some(&list));
            scroller.set_vexpand(true);
            scroller.set_min_content_height(320);
            scroller.set_has_frame(true);
            // A classic scrollbar beside the rows, so it never covers their remove buttons.
            scroller.set_overlay_scrolling(false);
            obj.append(&scroller);

            let search = gtk::SearchEntry::new();
            search.set_hexpand(true);
            search.set_placeholder_text(Some("Search actions or shortcuts"));
            obj.append(&search);

            let import = labelled_button("document-import-symbolic", "Import", "Import keybindings from a file");
            let export = labelled_button("document-export-symbolic", "Export", "Export keybindings to a file");
            let reset = labelled_button("document-revert-symbolic", "Reset", "Reset every binding to its default");
            let undo = labelled_button("edit-undo-symbolic", "Undo", "Restore the bindings from when preferences opened");
            undo.set_hexpand(true);
            undo.set_halign(gtk::Align::End);
            let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            for button in [&import, &export, &reset, &undo] {
                footer.append(button);
            }
            obj.append(&footer);

            let weak = obj.downgrade();
            add.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.add_binding())
            });
            list.connect_row_activated({
                let weak = weak.clone();
                move |_, row| with_list(&weak, |list| list.change_row(row.index()))
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
                    bindings.get(row.index() as usize).is_some_and(|(action, binding)| {
                        action.label.to_lowercase().contains(&needle)
                            || action.name.contains(&needle)
                            || binding.chord.contains(&needle)
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
            reset.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.confirm_reset())
            });
            undo.connect_clicked(move |_| with_list(&weak, |list| list.undo()));

            let _ = self.list.set(list);
            let _ = self.search.set(search);
            let _ = self.conflicts.set(conflicts);
            let _ = self.actions.set(actions);
            let _ = self.undo.set(undo);
        }
    }

    impl WidgetImpl for KeybindingList {}
    impl BoxImpl for KeybindingList {}

    /// Makes each item an action's name, and with `described`, its icon to the left, its description below in
    /// dimmed text, and its scope with the scope's icon at the top right.
    fn action_factory(described: bool) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            if !described {
                // The button: the icon, the name, then the description dimmed on the same line, cut short to fit.
                let description = gtk::Label::new(None);
                description.set_xalign(0.0);
                description.set_hexpand(true);
                description.set_ellipsize(gtk::pango::EllipsizeMode::End);
                description.set_width_chars(1);
                description.add_css_class("dim-label");
                description.add_css_class("action-description");
                let icon = gtk::Image::new();
                icon.set_pixel_size(16);
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                line.append(&icon);
                line.append(&name);
                line.append(&description);
                item.set_child(Some(&line));
                return;
            }
            name.set_hexpand(true);
            // The scope, small and dimmed, with its icon after it.
            let scope_label = gtk::Label::new(None);
            scope_label.add_css_class("action-scope");
            scope_label.add_css_class("dim-label");
            let scope_icon = gtk::Image::new();
            scope_icon.set_pixel_size(12);
            scope_icon.add_css_class("dim-label");
            let scope = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            scope.set_margin_start(24);
            scope.set_halign(gtk::Align::End);
            scope.set_valign(gtk::Align::Start);
            scope.append(&scope_label);
            scope.append(&scope_icon);
            let description = gtk::Label::new(None);
            description.set_xalign(0.0);
            description.set_wrap(true);
            description.set_max_width_chars(48);
            description.add_css_class("dim-label");
            description.add_css_class("action-description");
            let grid = gtk::Grid::builder().column_spacing(0).row_spacing(0).build();
            let icon = gtk::Image::new();
            icon.set_pixel_size(16);
            icon.set_valign(gtk::Align::Start);
            icon.set_margin_top(2);
            icon.set_margin_end(10);
            grid.attach(&icon, 0, 0, 1, 2);
            grid.attach(&name, 1, 0, 1, 1);
            grid.attach(&scope, 2, 0, 1, 1);
            grid.attach(&description, 1, 1, 2, 1);
            item.set_child(Some(&grid));
        });
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            // The filtered list moves items around, so the action is found by the name in its string.
            let action = item.item().and_downcast::<gtk::StringObject>().and_then(|object| {
                let text = object.string();
                let name = text.split('\n').next().unwrap_or_default().to_owned();
                ACTIONS.iter().find(|action| action.name == name)
            });
            let (Some(child), Some(action)) = (item.child(), action) else { return };
            if let Some(line) = child.downcast_ref::<gtk::Box>() {
                if let Some(icon) = line.first_child().and_downcast::<gtk::Image>() {
                    icon.set_icon_name(Some(action.icon));
                }
                if let Some(name) = line.first_child().and_then(|icon| icon.next_sibling()).and_downcast::<gtk::Label>() {
                    name.set_label(action.label);
                }
                if let Some(description) = line.last_child().and_downcast::<gtk::Label>() {
                    description.set_label(action.description);
                }
                return;
            }
            let Some(grid) = child.downcast_ref::<gtk::Grid>() else { return };
            let label = |column, row| grid.child_at(column, row).and_downcast::<gtk::Label>();
            if let Some(icon) = grid.child_at(0, 0).and_downcast::<gtk::Image>() {
                icon.set_icon_name(Some(action.icon));
            }
            if let (Some(name), Some(description)) = (label(1, 0), label(1, 1)) {
                name.set_label(action.label);
                description.set_label(action.description);
            }
            let scope = grid.child_at(2, 0);
            if let Some(scope_label) = scope.as_ref().and_then(|scope| scope.first_child()).and_downcast::<gtk::Label>() {
                scope_label.set_label(action.scope.title());
            }
            if let Some(scope_icon) = scope.as_ref().and_then(|scope| scope.last_child()).and_downcast::<gtk::Image>() {
                scope_icon.set_icon_name(Some(action.scope.icon()));
            }
        });
        factory
    }

    /// A button with an icon and a label, and a tooltip that says more.
    fn labelled_button(icon: &str, label: &str, tooltip: &str) -> gtk::Button {
        let content = adw::ButtonContent::builder().icon_name(icon).label(label).build();
        gtk::Button::builder().child(&content).tooltip_text(tooltip).build()
    }

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
        editor.imp().snapshot.replace(Some(keybindings::with_table(|table| table.clone())));
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
            for binding in keybindings::with_table(|table| table.bindings(action.name).to_vec()) {
                list.append(&self.binding_row(action, &binding));
                bindings.push((action, binding));
            }
        }
        imp.bindings.replace(bindings);
        list.invalidate_filter();
        self.update_conflicts();
        let changed = imp.snapshot.borrow().as_ref().is_some_and(|snapshot| keybindings::with_table(|table| *table != *snapshot));
        if let Some(undo) = imp.undo.get() {
            undo.set_sensitive(changed);
        }
    }

    /// Shows the first chord that is bound to more than one action whose scopes overlap, if there is one.
    fn update_conflicts(&self) {
        let imp = self.imp();
        let conflict = imp.bindings.borrow().iter().find_map(|(action, binding)| {
            let chord = &binding.chord;
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

    /// Saves the bindings file and shows the new rows.
    fn commit(&self) {
        if let Err(err) = keybindings::save() {
            eprintln!("rsimgv: could not save keybindings: {err}");
        }
        self.refresh();
    }

    /// Adds a chord to the action picked beside the add button.
    fn add_binding(&self) {
        let Some(picker) = self.imp().actions.get() else { return };
        let Some(action) = ACTIONS.get(picker.selected() as usize) else { return };
        let weak = self.downgrade();
        binding_dialog::show(self, action, None, move |chord, value| {
            keybindings::with_table(|table| table.add_with_value(action.name, &chord, value));
            if let Some(list) = weak.upgrade() {
                list.commit();
            }
        }, None);
    }

    /// Changes the chord of the binding on the row at `index`.
    fn change_row(&self, index: i32) {
        let Some((action, binding)) = self.imp().bindings.borrow().get(index as usize).cloned() else { return };
        let weak = self.downgrade();
        let previous = binding.chord.clone();
        let previous_for_delete = binding.chord.clone();
        binding_dialog::show(self, action, Some((binding.chord, binding.value)), move |new_chord, value| {
            keybindings::with_table(|table| {
                table.remove(action.name, &previous);
                table.add_with_value(action.name, &new_chord, value);
            });
            if let Some(list) = weak.upgrade() {
                list.commit();
            }
        }, Some(Box::new({
            let weak = self.downgrade();
            let chord = previous_for_delete;
            move || {
                if let Some(list) = weak.upgrade() {
                    list.remove_binding(action, &chord);
                }
            }
        })));
    }

    fn remove_binding(&self, action: &'static ActionInfo, chord: &str) {
        keybindings::with_table(|table| table.remove(action.name, chord));
        self.commit();
    }

    /// Asks before putting every binding back to its default.
    fn confirm_reset(&self) {
        use adw::prelude::AlertDialogExt;
        let dialog = adw::AlertDialog::new(
            Some("Reset Keybindings?"),
            Some("Every binding will be put back to its default. Undo can still restore the bindings from when preferences opened."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("reset", "Reset");
        dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.downgrade();
        dialog.connect_response(Some("reset"), move |_, _| {
            if let Some(list) = weak.upgrade() {
                list.reset();
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Puts the bindings back as they were when the editor opened.
    fn undo(&self) {
        let Some(snapshot) = self.imp().snapshot.borrow().clone() else { return };
        keybindings::with_table(|table| *table = snapshot);
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

    /// A row: the action's icon and label, the value it passes if it takes one, its chord in monospace, and its
    /// scope's icon, which removes it.
    fn binding_row(&self, action: &'static ActionInfo, binding: &Binding) -> gtk::ListBoxRow {
        let chord = binding.chord.as_str();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_top(2);
        content.set_margin_bottom(2);
        content.set_margin_start(12);

        content.append(&gtk::Image::from_icon_name(action.icon));
        let label = gtk::Label::new(Some(action.label));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        content.append(&label);

        if let Some(info) = value_info(action.name) {
            let value = binding.value.unwrap_or(info.default);
            let shown = gtk::Label::new(Some(&info.format(value)));
            shown.add_css_class("dim-label");
            content.append(&shown);
        }

        let key = gtk::Label::new(Some(chord));
        key.add_css_class("monospace");
        content.append(&key);

        // The scope's icon, which turns into a red remove button while the pointer is over it.
        let remove = gtk::Button::from_icon_name(action.scope.icon());
        remove.set_tooltip_text(Some("Remove binding"));
        remove.add_css_class("flat");
        remove.add_css_class("binding-remove");
        remove.set_valign(gtk::Align::Center);
        let hover = gtk::EventControllerMotion::new();
        hover.connect_enter({
            let remove = remove.clone();
            move |_, _, _| {
                remove.set_icon_name("user-trash-symbolic");
                remove.add_css_class("error");
            }
        });
        hover.connect_leave({
            let remove = remove.clone();
            move |_| {
                remove.set_icon_name(action.scope.icon());
                remove.remove_css_class("error");
            }
        });
        remove.add_controller(hover);
        let weak = self.downgrade();
        let removed = chord.to_owned();
        remove.connect_clicked(move |_| {
            if let Some(list) = weak.upgrade() {
                list.remove_binding(action, &removed);
            }
        });
        content.append(&remove);

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&content));
        // The tooltip: the name and action name, the description, then a little below, the scope with its icon.
        row.set_has_tooltip(true);
        row.connect_query_tooltip(move |_, _, _, _, tooltip| {
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            name.set_markup(&format!(
                "<b>{}</b> ({})",
                glib::markup_escape_text(action.label),
                glib::markup_escape_text(action.name),
            ));
            let scope_title = gtk::Label::new(None);
            scope_title.set_markup("<b>Scope:</b>");
            let scope = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            scope.append(&scope_title);
            scope.append(&gtk::Image::from_icon_name(action.scope.icon()));
            scope.append(&gtk::Label::new(Some(action.scope.title())));
            let description = gtk::Label::new(Some(action.description));
            description.set_xalign(0.0);
            description.set_wrap(true);
            description.set_max_width_chars(48);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            scope.set_margin_top(6);
            content.append(&name);
            content.append(&description);
            content.append(&scope);
            tooltip.set_custom(Some(&content));
            true
        });
        row
    }
}

