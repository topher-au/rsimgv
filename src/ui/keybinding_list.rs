//! The keybindings editor for the preferences: an action picker and add button, a line that flags conflicts,
//! a list of every binding with its scope, chord and a remove button, a search box, and import, export and
//! defaults, reset and undo. Activating a row changes its chord, and Delete removes it.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::keybindings::{
    self, actions::target_info, actions::value_info, actions::ActionInfo, actions::Scope, actions::ACTIONS, Binding, BindingTable,
};
use super::binding_dialog;
use super::key_handler::chord_from_event;
use super::keybind_label::KeybindLabel;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct KeybindingList {
        pub(super) list: OnceCell<gtk::ListBox>,
        pub(super) search: OnceCell<gtk::Entry>,
        /// Shares one width among the binding slots of every row.
        pub(super) binding_sizes: OnceCell<gtk::SizeGroup>,
        pub(super) search_clear: OnceCell<gtk::Button>,
        /// The box in the search field that holds the keybind the list is filtered to, as keycaps.
        pub(super) search_keybind: OnceCell<gtk::Box>,
        pub(super) conflicts: OnceCell<gtk::Label>,
        pub(super) actions: OnceCell<gtk::DropDown>,
        pub(super) reset: OnceCell<gtk::Button>,
        pub(super) undo: OnceCell<gtk::Button>,
        /// The bindings from before each change, the last one last, each with the binding that was selected then:
        /// what undo puts back, and selects again.
        pub(super) history: RefCell<Vec<(BindingTable, Option<(&'static str, String)>)>>,
        pub(super) keybind_button: OnceCell<gtk::Button>,
        pub(super) context_filter: OnceCell<adw::SplitButton>,
        /// The context the list is filtered to, or `None` for every context.
        pub(super) context: Cell<Option<Scope>>,
        /// Whether the next key combination becomes the keybind the list is filtered to.
        pub(super) capturing: Cell<bool>,
        /// The keybind the list is filtered to, if any.
        pub(super) keybind: RefCell<Option<String>>,
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
            // The popup opens on the current action, selected and scrolled into view, with the list focused rather
            // than the search. GTK clears the search as the popup opens, so this waits until that is done.
            if let Some(popup) = find_descendant::<gtk::Popover>(actions.upcast_ref()) {
                popup.connect_show({
                    let actions = actions.clone();
                    move |_| {
                        let actions = actions.clone();
                        glib::idle_add_local_once(move || select_current(&actions));
                    }
                });
            }
            // Typing while the list has focus starts the search: the key goes into the search box, which takes focus.
            if let (Some(list), Some(search)) = (
                find_descendant::<gtk::ListView>(actions.upcast_ref()),
                find_descendant::<gtk::SearchEntry>(actions.upcast_ref()),
            ) {
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |_, key, _, state| {
                    let modifiers = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK;
                    let Some(ch) = key.to_unicode().filter(|ch| !ch.is_control()) else { return glib::Propagation::Proceed };
                    if state.intersects(modifiers) {
                        return glib::Propagation::Proceed;
                    }
                    search.grab_focus();
                    let mut position = search.text().chars().count() as i32;
                    search.insert_text(&ch.to_string(), &mut position);
                    // Inserting leaves the caret where it was, so the next key would go in before this one.
                    search.set_position(-1);
                    glib::Propagation::Stop
                });
                list.add_controller(keys);
            }
            let add = icon_button("list-add-symbolic", "Add binding");

            // Always present, so the list does not move when a conflict appears or clears.
            let conflicts = gtk::Label::new(None);
            conflicts.set_justify(gtk::Justification::Center);
            conflicts.set_ellipsize(gtk::pango::EllipsizeMode::End);
            conflicts.set_size_request(-1, 24);
            conflicts.add_css_class("warning");

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            // A click only selects a row; double-clicking or pressing Enter opens its editor.
            list.set_activate_on_single_click(false);
            let scroller = gtk::ScrolledWindow::new();
            scroller.set_child(Some(&list));
            scroller.set_vexpand(true);
            scroller.set_min_content_height(320);
            scroller.set_has_frame(true);
            // A classic scrollbar beside the rows, so it never covers their remove buttons.
            scroller.set_overlay_scrolling(false);

            // The search field: the text, then the keybind it is filtered to as keycaps, and its buttons inside.
            let search = gtk::Entry::new();
            search.set_has_frame(false);
            search.set_hexpand(true);
            search.set_placeholder_text(Some("Search actions or shortcuts"));
            // The keybind's keys take the entry's place while the list is filtered to one.
            let keybind_chips = gtk::Box::new(gtk::Orientation::Horizontal, 3);
            keybind_chips.set_hexpand(true);
            keybind_chips.set_halign(gtk::Align::Start);
            keybind_chips.set_visible(false);
            let clear = icon_button("edit-clear-symbolic", "Clear the search and keybind");
            clear.add_css_class("flat");
            clear.set_visible(false);
            let keybind = icon_button("input-keyboard-symbolic", "Search by keybind");
            keybind.add_css_class("flat");
            let field = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            field.add_css_class("search-field");
            field.set_hexpand(true);
            field.append(&gtk::Image::from_icon_name("system-search-symbolic"));
            field.append(&search);
            field.append(&keybind_chips);
            field.append(&clear);
            field.append(&keybind);
            // The contexts to filter by: a split button that shows the chosen one. Its menu lists every context with
            // its icon, and its main button goes back to every context.
            let contexts = adw::SplitButton::new();
            contexts.set_tooltip_text(Some("Filter by context"));
            let options: Vec<(Option<Scope>, &'static str, &'static str)> = std::iter::once((None, "All contexts", "view-list-symbolic"))
                .chain(Scope::ALL.iter().map(|scope| (Some(*scope), scope.title(), scope.icon())))
                .collect();
            let context_menu = gtk::ListBox::new();
            context_menu.set_selection_mode(gtk::SelectionMode::None);
            for (_, title, icon) in &options {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                line.append(&gtk::Image::from_icon_name(icon));
                line.append(&gtk::Label::new(Some(title)));
                context_menu.append(&line);
            }
            let context_popover = gtk::Popover::new();
            context_popover.set_child(Some(&context_menu));
            contexts.set_popover(Some(&context_popover));
            context_menu.connect_row_activated({
                let weak = obj.downgrade();
                let popover = context_popover.clone();
                move |_, row| {
                    let scope = options[row.index() as usize].0;
                    popover.popdown();
                    with_list(&weak, |list| list.choose_context(scope));
                }
            });
            contexts.connect_clicked({
                let weak = obj.downgrade();
                move |_| with_list(&weak, |list| list.choose_context(None))
            });
            let _ = self.context_filter.set(contexts.clone());
            obj.choose_context(None);

            let import = labelled_button("document-import-symbolic", "Import", "Import keybindings from a file");
            let export = labelled_button("document-export-symbolic", "Export", "Export keybindings to a file");
            let defaults = labelled_button("document-revert-symbolic", "Defaults", "Put every binding back to its default");
            let reset = labelled_button("view-refresh-symbolic", "Reset", "Restore the bindings from when preferences opened");
            let undo = labelled_button("edit-undo-symbolic", "Undo", "Undo the last change");

            // The header row: the action picker, the add button, then a menu of the file and reset actions. Each menu
            // item closes the menu.
            let menu_items = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let menu_popover = gtk::Popover::new();
            for button in [&import, &export, &defaults, &reset] {
                button.add_css_class("flat");
                button.connect_clicked({
                    let popover = menu_popover.clone();
                    move |_| popover.popdown()
                });
                menu_items.append(button);
            }
            menu_popover.set_child(Some(&menu_items));
            let menu = gtk::MenuButton::new();
            menu.set_icon_name("view-more-symbolic");
            menu.set_tooltip_text(Some("More keybinding actions"));
            menu.set_popover(Some(&menu_popover));
            // The row above the list: the action picker, the add button, then undo.
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            top.append(&actions);
            top.append(&add);
            top.append(&undo);
            obj.append(&top);
            obj.append(&scroller);
            obj.append(&conflicts);

            // The row under the list: the context filter, the search field, then the menu.
            let filter = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            filter.append(&contexts);
            filter.append(&field);
            filter.append(&menu);
            filter.set_hexpand(true);
            obj.append(&filter);

            let weak = obj.downgrade();
            add.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.add_binding())
            });
            // Delete removes the selected binding.
            let delete = gtk::EventControllerKey::new();
            delete.connect_key_pressed({
                let weak = weak.clone();
                move |_, key, _, state| {
                    if chord_from_event(key, state).is_none_or(|chord| chord.format() != "delete") {
                        return glib::Propagation::Proceed;
                    }
                    with_list(&weak, |list| list.delete_selected());
                    glib::Propagation::Stop
                }
            });
            list.add_controller(delete);
            list.connect_row_activated({
                let weak = weak.clone();
                move |_, row| with_list(&weak, |list| list.change_row(row.index()))
            });
            search.connect_changed({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.update_search())
            });
            // A row shows when its binding has the keybind and context filtered to, if any, and its action or chord
            // matches the search text.
            list.set_filter_func({
                let weak = weak.clone();
                let search = search.clone();
                move |row| {
                    let Some(obj) = weak.upgrade() else { return true };
                    let imp = obj.imp();
                    let keybind = imp.keybind.borrow().clone();
                    let context = imp.context.get();
                    let needle = search.text().to_lowercase();
                    if keybind.is_none() && context.is_none() && needle.is_empty() {
                        return true;
                    }
                    let bindings = imp.bindings.borrow();
                    let Some((action, binding)) = bindings.get(row.index() as usize) else { return false };
                    keybind.as_ref().is_none_or(|chord| binding.chord == *chord)
                        && context.is_none_or(|scope| binding.context == scope)
                        && (needle.is_empty()
                            || action.label.to_lowercase().contains(&needle)
                            || action.name.contains(&needle)
                            || binding.chord.contains(&needle))
                }
            });
            keybind.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.start_keybind_capture())
            });
            clear.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.clear_search())
            });
            // While capturing, the next key combination is the keybind to find. Escape clears the filter instead.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed({
                let weak = weak.clone();
                move |_, key, _, state| {
                    let Some(editor) = weak.upgrade() else { return glib::Propagation::Proceed };
                    if !editor.imp().capturing.get() {
                        return glib::Propagation::Proceed;
                    }
                    // A lone modifier is not a combination yet, so it is ignored.
                    if let Some(chord) = chord_from_event(key, state) {
                        editor.capture_keybind((chord.key != "escape").then(|| chord.format()));
                    }
                    glib::Propagation::Stop
                }
            });
            obj.add_controller(keys);
            import.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.import())
            });
            export.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.export())
            });
            defaults.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.confirm_defaults())
            });
            reset.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.reset())
            });
            undo.connect_clicked({
                let weak = weak.clone();
                move |_| with_list(&weak, |list| list.undo())
            });

            let _ = self.list.set(list);
            let _ = self.search.set(search);
            let _ = self.conflicts.set(conflicts);
            let _ = self.actions.set(actions);
            let _ = self.reset.set(reset);
            let _ = self.undo.set(undo);
            let _ = self.keybind_button.set(keybind);
            let _ = self.search_clear.set(clear);
            let _ = self.binding_sizes.set(gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal));
            let _ = self.search_keybind.set(keybind_chips);
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
                let titles: Vec<&str> = action.contexts().iter().map(|context| context.title()).collect();
                scope_label.set_label(&titles.join(", "));
            }
            if let Some(scope_icon) = scope.as_ref().and_then(|scope| scope.last_child()).and_downcast::<gtk::Image>() {
                scope_icon.set_icon_name(Some(action.default_context().icon()));
            }
        });
        factory
    }

    /// Selects the current action in the picker's popup, scrolls it into view and focuses the list. The list
    /// has no size until the popup is laid out, so the scroll waits for the first frame that gives it one.
    fn select_current(actions: &gtk::DropDown) {
        let Some(list) = find_descendant::<gtk::ListView>(actions.upcast_ref()) else { return };
        list.grab_focus();
        let actions = actions.clone();
        list.add_tick_callback(move |list, _| {
            if list.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            list.scroll_to(actions.selected(), gtk::ListScrollFlags::SELECT, None);
            glib::ControlFlow::Break
        });
    }

    /// The first widget of type `T` below `widget`, searched depth first.
    fn find_descendant<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = current.downcast_ref::<T>() {
                return Some(found.clone());
            }
            if let Some(found) = find_descendant::<T>(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
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
                let clashes = keybindings::with_table(|table| table.conflicts(action.name, &binding.chord, binding.context));
                list.append(&self.binding_row(action, &binding, &clashes));
                bindings.push((action, binding));
            }
        }
        imp.bindings.replace(bindings);
        list.invalidate_filter();
        self.update_conflicts();
        let changed = imp.snapshot.borrow().as_ref().is_some_and(|snapshot| keybindings::with_table(|table| *table != *snapshot));
        if let Some(reset) = imp.reset.get() {
            reset.set_sensitive(changed);
        }
        if let Some(undo) = imp.undo.get() {
            undo.set_sensitive(!imp.history.borrow().is_empty());
        }
    }

    /// Waits for the next key combination, which becomes the keybind the list is filtered to.
    fn start_keybind_capture(&self) {
        self.imp().capturing.set(true);
        self.update_search();
    }

    /// Ends the capture: `chord` is the keybind to filter to, or `None` to clear it.
    fn capture_keybind(&self, chord: Option<String>) {
        let imp = self.imp();
        imp.capturing.set(false);
        imp.keybind.replace(chord);
        self.update_search();
    }

    /// Empties the search text and the keybind, so the list shows every binding again.
    fn clear_search(&self) {
        let imp = self.imp();
        if let Some(search) = imp.search.get() {
            search.set_text("");
        }
        imp.keybind.replace(None);
        self.update_search();
    }

    /// Shows the search field's state, then filters the list again: the keybind it is filtered to as keycaps, its
    /// clear button while it has text or a keybind, its keybind button lit while capturing, and its placeholder.
    fn update_search(&self) {
        let imp = self.imp();
        let keybind = imp.keybind.borrow().clone();
        let capturing = imp.capturing.get();
        if let Some(chips) = imp.search_keybind.get() {
            while let Some(child) = chips.first_child() {
                chips.remove(&child);
            }
            if let Some(chord) = &keybind {
                chips.append(&KeybindLabel::new(chord));
            }
        }
        let has_text = imp.search.get().is_some_and(|search| !search.text().is_empty());
        let clearable = has_text || keybind.is_some();
        // The clear button takes the keybind button's place while there is something to clear.
        if let Some(clear) = imp.search_clear.get() {
            clear.set_visible(clearable);
        }
        if let Some(button) = imp.keybind_button.get() {
            button.set_visible(!clearable);
        }
        // The keybind's keys replace the entry while the list is filtered to one.
        if let Some(search) = imp.search.get() {
            search.set_visible(keybind.is_none());
        }
        if let Some(chips) = imp.search_keybind.get() {
            chips.set_visible(keybind.is_some());
        }
        if let Some(search) = imp.search.get() {
            let placeholder = if capturing { "Press a key combination\u{2026}" } else { "Search actions or shortcuts" };
            search.set_placeholder_text(Some(placeholder));
        }
        if let Some(button) = imp.keybind_button.get() {
            if capturing {
                button.add_css_class("suggested-action");
                button.set_tooltip_text(Some("Press the keys to find their actions, or Escape to clear the filter"));
            } else {
                button.remove_css_class("suggested-action");
                button.set_tooltip_text(Some("Search by keybind"));
            }
        }
        if let Some(list) = imp.list.get() {
            list.invalidate_filter();
        }
    }

    /// Filters the list to one context, or to every context for `None`, and shows that on the context button.
    fn choose_context(&self, scope: Option<Scope>) {
        let imp = self.imp();
        imp.context.set(scope);
        if let Some(button) = imp.context_filter.get() {
            let (title, icon) = scope.map_or(("All contexts", "view-list-symbolic"), |scope| (scope.title(), scope.icon()));
            button.set_label(title);
            button.set_icon_name(icon);
        }
        if let Some(list) = imp.list.get() {
            list.invalidate_filter();
        }
    }

    /// Shows the first chord that is bound to more than one action whose scopes overlap, if there is one.
    fn update_conflicts(&self) {
        let imp = self.imp();
        let conflict = imp.bindings.borrow().iter().find_map(|(action, binding)| {
            let chord = &binding.chord;
            let others = keybindings::with_table(|table| table.conflicts(action.name, chord, binding.context));
            (!others.is_empty()).then(|| {
                let names: Vec<&str> = std::iter::once(action.name).chain(others.iter().map(|(other, _)| other.name)).collect();
                format!("Conflicts - {chord} in {}: {}", binding.context.title().to_lowercase(), names.join(", "))
            })
        });
        let label = imp.conflicts.get().expect("conflicts is built in constructed");
        label.set_label(conflict.as_deref().unwrap_or_default());
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
        binding_dialog::show(self, action, None, move |binding| {
            if let Some(list) = weak.upgrade() {
                list.edit(|table| {
                    table.insert(action.name, binding);
                });
            }
        }, None);
    }

    /// Changes the chord of the binding on the row at `index`.
    fn change_row(&self, index: i32) {
        let Some((action, binding)) = self.imp().bindings.borrow().get(index as usize).cloned() else { return };
        let weak = self.downgrade();
        let previous = binding.chord.clone();
        let previous_for_delete = binding.chord.clone();
        binding_dialog::show(self, action, Some(binding), move |new_binding| {
            if let Some(list) = weak.upgrade() {
                let new_chord = new_binding.chord.clone();
                list.edit(|table| {
                    table.remove(action.name, &previous);
                    table.insert(action.name, new_binding);
                });
                // The rows are rebuilt by the change, so the edited binding is selected again.
                list.select_binding(action.name, &new_chord);
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
        self.edit(|table| table.remove(action.name, chord));
    }

    /// Deletes the selected binding, then selects the binding after it, or the one before it at the end of the list.
    fn delete_selected(&self) {
        let Some(list) = self.imp().list.get() else { return };
        let Some(row) = list.selected_row() else { return };
        let index = row.index();
        let Some((action, binding)) = self.imp().bindings.borrow().get(index as usize).cloned() else { return };
        self.edit(|table| table.remove(action.name, &binding.chord));
        self.select_near(index);
    }

    /// Selects the first visible row from `index` on, or else the last visible row before it.
    fn select_near(&self, index: i32) {
        let Some(list) = self.imp().list.get() else { return };
        let count = self.imp().bindings.borrow().len() as i32;
        let rows: Vec<gtk::ListBoxRow> = (0..count).filter_map(|i| list.row_at_index(i)).collect();
        let index = index as usize;
        let after = rows.iter().skip(index).find(|row| row.is_child_visible());
        let before = rows.iter().take(index).rev().find(|row| row.is_child_visible());
        if let Some(row) = after.or(before) {
            list.select_row(Some(row));
        }
    }

    /// Asks before putting every binding back to its default.
    fn confirm_defaults(&self) {
        use adw::prelude::AlertDialogExt;
        let dialog = adw::AlertDialog::new(
            Some("Restore Defaults?"),
            Some("Every binding will be put back to its default. Undo can still bring back the bindings from before."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("defaults", "Restore");
        dialog.set_response_appearance("defaults", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.downgrade();
        dialog.connect_response(Some("defaults"), move |_, _| {
            if let Some(list) = weak.upgrade() {
                list.restore_defaults();
            }
        });
        adw::prelude::AdwDialogExt::present(&dialog, Some(self));
    }

    /// Puts every binding back to its default.
    fn restore_defaults(&self) {
        self.edit(|table| table.reset_all());
    }

    /// Puts the bindings back as they were when the editor opened.
    fn reset(&self) {
        let Some(snapshot) = self.imp().snapshot.borrow().clone() else { return };
        self.edit(|table| *table = snapshot);
    }

    /// Puts back the bindings from before the last change, and selects the binding that was selected then.
    fn undo(&self) {
        let Some((previous, selected)) = self.imp().history.borrow_mut().pop() else { return };
        keybindings::with_table(|table| *table = previous);
        self.commit();
        if let Some((name, chord)) = selected {
            self.select_binding(name, &chord);
        }
    }

    /// Makes a change to the bindings. The bindings from before it are kept, with the selected binding, so undo can
    /// put them back, then they are saved and shown. A change that leaves the bindings as they were is not kept.
    fn edit(&self, change: impl FnOnce(&mut BindingTable)) {
        let before = keybindings::with_table(|table| table.clone());
        let selected = self.selected_binding();
        keybindings::with_table(change);
        if keybindings::with_table(|table| *table != before) {
            self.imp().history.borrow_mut().push((before, selected));
        }
        self.commit();
    }

    /// The action's name and the chord of the selected row's binding, if a row is selected.
    fn selected_binding(&self) -> Option<(&'static str, String)> {
        let row = self.imp().list.get()?.selected_row()?;
        let bindings = self.imp().bindings.borrow();
        let (action, binding) = bindings.get(row.index() as usize)?;
        Some((action.name, binding.chord.clone()))
    }

    /// Selects the row of the binding of the action `name` with `chord`, if there is one.
    fn select_binding(&self, name: &str, chord: &str) {
        let Some(list) = self.imp().list.get() else { return };
        let index = self.imp().bindings.borrow().iter().position(|(action, binding)| action.name == name && binding.chord == chord);
        if let Some(row) = index.and_then(|index| list.row_at_index(index as i32)) {
            list.select_row(Some(&row));
        }
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
                    if let Some(list) = weak.upgrade() {
                        list.edit(|table| {
                            for problem in table.apply_yaml(&text) {
                                eprintln!("rsimgv: keybindings: {problem}");
                            }
                        });
                    }
                }
                Err(err) => eprintln!("rsimgv: could not read {}: {err}", path.display()),
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

    /// A row: the action's icon and label, the value it passes if it takes one, then the context it is active in and
    /// the binding's keys.
    fn binding_row(&self, action: &'static ActionInfo, binding: &Binding, clashes: &[(&'static ActionInfo, Scope)]) -> gtk::ListBoxRow {
        // The tooltip outlives this call, so it keeps its own copy.
        let clashes = clashes.to_vec();
        let chord = binding.chord.as_str();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_top(2);
        content.set_margin_bottom(2);
        content.set_margin_start(12);

        // The target the binding chooses, for an action that takes one, as it is called.
        let target = target_info(action.name).map(|info| info.title(binding.target.as_deref()));
        // The value the binding passes, for an action that takes one, as its unit shows it.
        let value = value_info(action.name).map(|info| (info, info.format(binding.value.unwrap_or(info.default))));
        // The script the binding runs, for the action that runs one, or the message of the notification it shows.
        let script = binding.script.clone().or_else(|| binding.notification.as_ref().map(|notification| notification.message.clone()));
        let notification = binding.notification.clone();

        let icon = gtk::Image::from_icon_name(action.icon);
        if !clashes.is_empty() {
            icon.add_css_class("conflict");
        }
        content.append(&icon);
        let label = gtk::Label::new(None);
        let title = glib::markup_escape_text(action.label);
        // The target, value or script follows the title in brackets, dimmed.
        let mut details: Vec<String> = target.map(str::to_owned).into_iter().collect();
        match (&value, &script) {
            (Some((_, shown)), _) => details.push(shown.clone()),
            (None, Some(script)) => details.push(script.clone()),
            (None, None) => {}
        }
        if details.is_empty() {
            label.set_markup(&title);
        } else {
            label.set_markup(&format!("{title} <span alpha=\"55%\">({})</span>", glib::markup_escape_text(&details.join(", "))));
        }
        label.set_xalign(0.0);
        label.set_hexpand(true);
        if !clashes.is_empty() {
            label.add_css_class("conflict");
        }
        content.append(&label);

        // The context has a fixed width, with its icon before its name. The binding is right-aligned in a slot that
        // shares its width with every other row's, so the column is as wide as its widest binding.
        let context = binding.context;
        let context_cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        context_cell.add_css_class("binding-context");
        context_cell.set_width_request(130);
        // The cell keeps its width, and its name fills it, pushed to the right.
        context_cell.set_hexpand(false);
        let context_label = gtk::Label::new(Some(context.title()));
        context_label.add_css_class("dim-label");
        context_label.set_xalign(1.0);
        context_label.set_hexpand(true);
        context_cell.append(&context_label);
        content.append(&context_cell);

        let binding_keys = KeybindLabel::new(chord);
        binding_keys.set_halign(gtk::Align::End);
        // A box packs its children from the start, so an expanding spacer before the keys pushes them to the end.
        let binding_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // The slot must not expand itself: a child that expands would otherwise make it take a share of the row's
        // spare width, which the name should have to itself.
        binding_slot.set_hexpand(false);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        binding_slot.append(&spacer);
        // A conflicting binding has a warning sign to the left of its keys, and its keys are yellow.
        if !clashes.is_empty() {
            let warning = gtk::Label::new(Some("\u{26a0}"));
            warning.add_css_class("conflict");
            binding_slot.append(&warning);
            binding_keys.add_css_class("conflict");
        }
        binding_slot.append(&binding_keys);
        if let Some(sizes) = self.imp().binding_sizes.get() {
            sizes.add_widget(&binding_slot);
        }
        content.append(&binding_slot);

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
            scope_title.set_markup("<b>Active in:</b>");
            let scope = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            scope.append(&scope_title);
            scope.append(&gtk::Image::from_icon_name(context.icon()));
            scope.append(&gtk::Label::new(Some(context.title())));
            let description = gtk::Label::new(Some(action.description));
            description.set_xalign(0.0);
            description.set_wrap(true);
            description.set_max_width_chars(48);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            scope.set_margin_top(6);
            content.append(&name);
            if let Some(notification) = &notification {
                let line = gtk::Label::new(None);
                line.set_xalign(0.0);
                line.set_markup(&format!(
                    "<b>Message:</b> {} <span alpha=\"55%\">({}, {} s)</span>",
                    glib::markup_escape_text(&notification.message),
                    glib::markup_escape_text(&notification.level),
                    notification.seconds,
                ));
                content.append(&line);
            } else if let Some(script) = &script {
                let line = gtk::Label::new(None);
                line.set_xalign(0.0);
                line.set_markup(&format!("<b>Script:</b> {}", glib::markup_escape_text(script)));
                content.append(&line);
            }
            if let Some((info, shown)) = &value {
                let line = gtk::Label::new(None);
                line.set_xalign(0.0);
                line.set_markup(&format!("<b>{}:</b> {}", glib::markup_escape_text(info.label), glib::markup_escape_text(shown)));
                content.append(&line);
            }
            content.append(&description);
            content.append(&scope);
            // The bindings this one clashes with, each with its context.
            if !clashes.is_empty() {
                let heading = gtk::Label::new(None);
                heading.set_xalign(0.0);
                heading.set_markup("<b>Conflicts</b>");
                content.append(&heading);
                for (other, other_context) in &clashes {
                    let line = gtk::Label::new(None);
                    line.set_xalign(0.0);
                    line.set_markup(&format!(
                        "<span foreground=\"#f6d32d\">\u{26a0}</span> {} <span alpha=\"55%\">({}, {})</span>",
                        glib::markup_escape_text(other.label),
                        glib::markup_escape_text(other.name),
                        other_context.title(),
                    ));
                    content.append(&line);
                }
            }
            tooltip.set_custom(Some(&content));
            true
        });
        row
    }
}

