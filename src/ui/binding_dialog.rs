//! The dialog that adds or changes a binding of an action: a chord captured from the keyboard, the context the
//! binding is active in, for an action that takes a value, the value the binding passes, for the action that runs a
//! script, the script, and for the action that shows a notification, its message, level and duration. It warns when
//! the chord is already bound to an action whose context overlaps.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::keybindings::actions::{
    value_info, ActionInfo, Scope, DEFAULT_NOTIFICATION_SECONDS, MAX_NOTIFICATION_SECONDS, NOTIFICATION_LEVELS,
};
use crate::scripts::read_scripts;
use crate::keybindings::{self, chord::Chord, Binding, Notification};
use super::key_handler::chord_from_event;
use super::keybind_label::KeybindLabel;

/// Opens the dialog over `parent` for `action`. `existing` is the binding being changed, or `None` to add one.
/// `on_save` receives the binding as the dialog leaves it, with the chord as typed. With `on_delete`, the dialog also
/// offers to delete the binding.
pub fn show(
    parent: &impl IsA<gtk::Widget>,
    action: &'static ActionInfo,
    existing: Option<Binding>,
    on_save: impl Fn(Binding) + 'static,
    on_delete: Option<Box<dyn Fn()>>,
) {
    let existing_value = existing.as_ref().and_then(|binding| binding.value);
    let existing_context = existing.as_ref().map(|binding| binding.context);
    let existing_script = existing.as_ref().and_then(|binding| binding.script.clone());
    let existing_notification = existing.as_ref().and_then(|binding| binding.notification.clone());
    let existing = existing.map(|binding| binding.chord);
    let heading = if existing.is_some() { "Change Binding" } else { "Add Binding" };
    let dialog = adw::AlertDialog::new(Some(heading), Some(action.label));
    if on_delete.is_some() {
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    }
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let chord: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(existing.clone()));
    // A new binding starts by waiting for its key; an existing one waits for a click on its chord.
    let capturing = Rc::new(Cell::new(existing.is_none()));
    let capture = gtk::Button::new();
    let conflict = gtk::Label::new(None);
    conflict.set_wrap(true);
    conflict.set_xalign(0.0);
    conflict.add_css_class("warning");
    conflict.set_visible(false);

    // The context the binding is active in, one of the action's contexts, starting at the binding's own or the
    // action's default. A change to it can clash the chord with another action, so it refreshes the warning.
    let titles: Vec<&str> = action.contexts().iter().map(|context| context.title()).collect();
    let contexts = gtk::DropDown::from_strings(&titles);
    let start = existing_context.unwrap_or(action.default_context());
    contexts.set_selected(action.contexts().iter().position(|context| *context == start).unwrap_or(0) as u32);
    contexts.set_hexpand(true);

    // The scripts to pick from, for the action that runs one: those on the Scripts page, and the one the binding
    // already runs if it has since been removed from there, so changing the binding does not silently swap it.
    let scripts = action.takes_script().then(|| {
        let settings = gio::Settings::new("au.topher.rimgv");
        let mut names: Vec<String> = read_scripts(&settings).into_iter().map(|script| script.name).collect();
        if let Some(script) = existing_script.as_ref().filter(|script| !names.contains(script)) {
            names.push(script.clone());
        }
        names
    });
    let script_picker = scripts.as_ref().map(|names| {
        let shown: Vec<&str> = names.iter().map(String::as_str).collect();
        let picker = gtk::DropDown::from_strings(&shown);
        picker.set_hexpand(true);
        if let Some(index) = existing_script.as_ref().and_then(|script| names.iter().position(|name| name == script)) {
            picker.set_selected(index as u32);
        }
        picker
    });
    // A binding of the script action cannot be saved without a script to run.
    let can_save = scripts.as_ref().is_none_or(|names| !names.is_empty());

    // The notification, for the action that shows one: its message, level and how long it stays.
    let notification_fields = action.takes_notification().then(|| {
        let message = gtk::Entry::new();
        message.set_placeholder_text(Some("Message"));
        message.set_hexpand(true);
        let level = gtk::DropDown::from_strings(&["Info", "Success", "Warning", "Error", "Plain"]);
        level.set_hexpand(true);
        let seconds = gtk::SpinButton::with_range(0.5, MAX_NOTIFICATION_SECONDS, 0.5);
        seconds.set_digits(1);
        seconds.set_hexpand(true);
        match &existing_notification {
            Some(notification) => {
                message.set_text(&notification.message);
                level.set_selected(NOTIFICATION_LEVELS.iter().position(|name| *name == notification.level).unwrap_or(0) as u32);
                seconds.set_value(notification.seconds);
            }
            None => seconds.set_value(DEFAULT_NOTIFICATION_SECONDS),
        }
        (message, level, seconds)
    });

    let refresh = {
        let dialog = dialog.clone();
        let chord = chord.clone();
        let capture = capture.clone();
        let conflict = conflict.clone();
        let contexts = contexts.clone();
        let message_field = notification_fields.as_ref().map(|(message, ..)| message.clone());
        Rc::new(move || {
            let Some(text) = chord.borrow().clone() else {
                capture.set_child(Some(&gtk::Label::new(Some("Press a key\u{2026}"))));
                dialog.set_response_enabled("save", false);
                conflict.set_visible(false);
                return;
            };
            capture.set_child(Some(&KeybindLabel::new(&text)));
            // A notification cannot be saved without a message to show.
            let has_message = message_field.as_ref().is_none_or(|message| !message.text().trim().is_empty());
            dialog.set_response_enabled("save", can_save && has_message);
            let clashes = keybindings::with_table(|table| table.conflicts(action.name, &text, selected_context(action, &contexts)));
            conflict.set_visible(!clashes.is_empty());
            let names: Vec<&str> = clashes.iter().map(|(clash, _)| clash.label).collect();
            conflict.set_label(&format!("Already bound to {}", names.join(", ")));
        })
    };
    refresh();
    {
        let refresh = refresh.clone();
        contexts.connect_selected_notify(move |_| refresh());
    }
    if let Some((message, ..)) = &notification_fields {
        let refresh = refresh.clone();
        message.connect_changed(move |_| refresh());
    }

    {
        let capturing = capturing.clone();
        capture.connect_clicked(move |_| capturing.set(true));
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&capture);
    let context_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    context_row.append(&gtk::Label::new(Some("Active in")));
    context_row.append(&contexts);
    content.append(&context_row);
    if let (Some(picker), Some(names)) = (&script_picker, &scripts) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Label::new(Some("Script")));
        if names.is_empty() {
            let hint = gtk::Label::new(Some("There are no scripts yet. Add one on the Scripts page."));
            hint.set_wrap(true);
            hint.set_xalign(0.0);
            hint.add_css_class("dim-label");
            row.append(&hint);
        } else {
            row.append(picker);
        }
        content.append(&row);
    }
    if let Some((message, level, seconds)) = &notification_fields {
        for (title, field) in [("Message", message.upcast_ref::<gtk::Widget>()), ("Level", level.upcast_ref()), ("Seconds", seconds.upcast_ref())] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Label::new(Some(title)));
            row.append(field);
            content.append(&row);
        }
    }
    // The value, for an action that takes one, starting at the binding's own or the action's default.
    let value = value_info(action.name).map(|info| {
        let spin = gtk::SpinButton::with_range(info.min, info.max, info.step);
        spin.set_digits(info.digits);
        spin.set_value(existing_value.unwrap_or(info.default));
        spin.set_hexpand(true);
        let label = gtk::Label::new(Some(info.label));
        // The value as it will be shown, in its unit, beside the field.
        let shown = gtk::Label::new(Some(&info.format(spin.value())));
        shown.add_css_class("dim-label");
        shown.set_width_chars(12);
        shown.set_xalign(0.0);
        spin.connect_value_changed({
            let shown = shown.clone();
            move |spin| shown.set_label(&info.format(spin.value()))
        });
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&label);
        row.append(&spin);
        row.append(&shown);
        content.append(&row);
        spin
    });
    content.append(&conflict);
    dialog.set_extra_child(Some(&content));

    // While capturing, the next key press is the chord. Other presses go to the dialog as usual.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let chord = chord.clone();
        let capturing = capturing.clone();
        let refresh = refresh.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if !capturing.get() {
                return glib::Propagation::Proceed;
            }
            let Some(captured) = chord_from_event(key, state) else { return glib::Propagation::Stop };
            capturing.set(false);
            chord.replace(Some(Chord::format(&captured)));
            refresh();
            glib::Propagation::Stop
        });
    }
    dialog.add_controller(keys);

    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            if let Some(on_delete) = &on_delete {
                on_delete();
            }
            return;
        }
        if response != "save" {
            return;
        }
        let Some(text) = chord.borrow().clone() else { return };
        let script = script_picker.as_ref().zip(scripts.as_ref()).and_then(|(picker, names)| names.get(picker.selected() as usize).cloned());
        let notification = notification_fields.as_ref().map(|(message, level, seconds)| Notification {
            message: message.text().trim().to_owned(),
            level: NOTIFICATION_LEVELS.get(level.selected() as usize).copied().unwrap_or("info").to_owned(),
            seconds: seconds.value(),
        });
        on_save(Binding {
            chord: text,
            context: selected_context(action, &contexts),
            value: value.as_ref().map(gtk::SpinButton::value),
            script,
            notification,
        });
    });
    adw::prelude::AdwDialogExt::present(&dialog, Some(parent.as_ref()));
}

/// The context the dropdown shows, one of the action's contexts.
fn selected_context(action: &ActionInfo, contexts: &gtk::DropDown) -> Scope {
    action.contexts().get(contexts.selected() as usize).copied().unwrap_or(action.default_context())
}
