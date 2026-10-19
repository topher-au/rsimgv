//! The keybinding handler: one capture-phase key controller on the window. Each key press becomes a chord, the
//! chord is resolved against the window's active scopes, and the action it names is activated. Modeled on
//! pyimgv-gtk's `ActionKeyHandler`: a key press is left to the dialog or the text field that has it, and a
//! plain key typed into a text field is text, not a command.

use adw::prelude::AdwApplicationWindowExt;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::keybindings::{self, chord::Chord};
use super::Window;

/// Routes key presses on a window to the actions their chords are bound to.
pub struct ActionKeyHandler {
    window: glib::WeakRef<Window>,
    controller: gtk::EventControllerKey,
}

impl ActionKeyHandler {
    /// Attaches a handler to `window`, which keeps it until `uninstall`.
    pub fn install(window: &Window) -> Self {
        let controller = gtk::EventControllerKey::new();
        // Captured before the focused widget sees the key, so bindings win over widgets' own handling.
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = window.downgrade();
        controller.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(window) => handle(&window, key, state),
            None => glib::Propagation::Proceed,
        });
        window.add_controller(controller.clone());
        Self { window: window.downgrade(), controller }
    }

    /// Detaches the handler from its window.
    pub fn uninstall(&self) {
        if let Some(window) = self.window.upgrade() {
            window.remove_controller(&self.controller);
        }
    }
}

/// The chords that open GTK's inspector: Ctrl+Shift+I, and Ctrl+Shift+D.
const GTK_INSPECTOR_CHORDS: &[&str] = &["ctrl+shift+i", "ctrl+shift+d"];

/// Runs the action bound to the key press, if the window's state lets it. A key with no binding in the active
/// scopes is dropped silently: the handler consumes it, and widgets never see it. The exceptions are text fields,
/// where typing and editing keys belong to the entry, and an open dialog, which owns every key.
fn handle(window: &Window, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    let Some(chord) = chord_from_event(key, state) else { return glib::Propagation::Proceed };
    // GTK's inspector is always reached, whatever the bindings say, so it stays available for debugging.
    if GTK_INSPECTOR_CHORDS.contains(&chord.format().as_str()) {
        return glib::Propagation::Proceed;
    }
    // An open dialog owns the keys: the keybinding editor must be able to capture a chord that is already bound.
    if window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    // A plain key typed into a text field is text, not a command.
    let typing = is_text_key(state)
        && gtk::prelude::GtkWindowExt::focus(window).is_some_and(|widget| widget.dynamic_cast_ref::<gtk::Editable>().is_some());
    if typing {
        return glib::Propagation::Proceed;
    }
    let scopes = window.active_scopes();
    let Some(action) = keybindings::with_table(|table| table.resolve(&chord.format(), &scopes)) else {
        return glib::Propagation::Stop;
    };
    if gtk::prelude::WidgetExt::activate_action(window, action.name, None).is_ok() {
        glib::Propagation::Stop
    } else {
        glib::Propagation::Proceed
    }
}

pub fn chord_from_event(key: gdk::Key, state: gdk::ModifierType) -> Option<Chord> {
    let name = key.to_lower().name()?;
    if MODIFIER_KEYS.contains(&name.as_str()) {
        return None;
    }
    Some(Chord {
        key: name.to_ascii_lowercase(),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        super_key: state.contains(gdk::ModifierType::SUPER_MASK),
    })
}

const MODIFIER_KEYS: &[&str] = &[
    "Shift_L", "Shift_R", "Control_L", "Control_R", "Alt_L", "Alt_R", "Super_L", "Super_R", "Meta_L", "Meta_R",
    "ISO_Level3_Shift", "Caps_Lock",
];

/// Whether a plain key (no Ctrl, Alt or Super) should type into a text field rather than run a binding.
pub fn is_text_key(state: gdk::ModifierType) -> bool {
    !state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK)
}
