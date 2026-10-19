//! The dialog that adds or changes a binding of an action: a chord captured from the keyboard, and for an
//! action that takes a value, the value the binding passes. It warns when
//! the chord is already bound to an action whose scope overlaps.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::keybindings::actions::{value_info, ActionInfo};
use crate::keybindings::{self, chord::Chord};
use super::key_handler::chord_from_event;

/// Opens the dialog over `parent` for `action`. `existing` is the chord being changed and its value, or `None`
/// to add one. `on_save` receives the canonical chord and, for an action that takes one, the value. With
/// `on_delete`, the dialog also offers to delete the binding.
pub fn show(
    parent: &impl IsA<gtk::Widget>,
    action: &'static ActionInfo,
    existing: Option<(String, Option<f64>)>,
    on_save: impl Fn(String, Option<f64>) + 'static,
    on_delete: Option<Box<dyn Fn()>>,
) {
    let existing_value = existing.as_ref().and_then(|(_, value)| *value);
    let existing = existing.map(|(chord, _)| chord);
    let heading = if existing.is_some() { "Change Binding" } else { "Add Binding" };
    let body = format!("{} \u{b7} {}", action.scope.title(), action.label);
    let dialog = adw::AlertDialog::new(Some(heading), Some(&body));
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
    capture.add_css_class("monospace");
    let conflict = gtk::Label::new(None);
    conflict.set_wrap(true);
    conflict.set_xalign(0.0);
    conflict.add_css_class("warning");
    conflict.set_visible(false);

    let refresh = {
        let dialog = dialog.clone();
        let chord = chord.clone();
        let capture = capture.clone();
        let conflict = conflict.clone();
        Rc::new(move || {
            let Some(text) = chord.borrow().clone() else {
                capture.set_label("Press a key\u{2026}");
                dialog.set_response_enabled("save", false);
                conflict.set_visible(false);
                return;
            };
            capture.set_label(&text);
            dialog.set_response_enabled("save", true);
            let clashes = keybindings::with_table(|table| table.conflicts(action.name, &text));
            conflict.set_visible(!clashes.is_empty());
            let names: Vec<&str> = clashes.iter().map(|clash| clash.label).collect();
            conflict.set_label(&format!("Already bound to {}", names.join(", ")));
        })
    };
    refresh();

    {
        let capturing = capturing.clone();
        capture.connect_clicked(move |_| capturing.set(true));
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&capture);
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
        on_save(text, value.as_ref().map(gtk::SpinButton::value));
    });
    adw::prelude::AdwDialogExt::present(&dialog, Some(parent.as_ref()));
}
