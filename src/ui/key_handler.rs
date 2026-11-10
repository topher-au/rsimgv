//! The keybinding handler. Each key press becomes a chord, the chord is resolved against the window's active scopes,
//! and the action it names is activated. Who gets the key first depends on where the keyboard focus is:
//!
//! - Nothing that takes keys has the focus: the window's bindings take the key before the focused widget sees it
//!   (a capture-phase controller). A key with no binding is swallowed, as before.
//! - A text field or a popover has the focus: the widget takes its keys first, so Ctrl+C, Ctrl+V, Ctrl+A and typing
//!   stay with the field, and a binding acts only on a key the widget leaves (a bubble-phase controller).
//! - A dialog is open: it owns every key, and no binding acts.
//!
//! Modeled on pyimgv-gtk's `ActionKeyHandler`.

use adw::prelude::AdwApplicationWindowExt;
use gtk::glib::prelude::StaticType;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::keybindings::{self, chord::Chord};
use super::Window;

/// Routes key presses on a window to the actions their chords are bound to. Both controllers stay attached until
/// `uninstall`.
pub struct ActionKeyHandler {
    window: glib::WeakRef<Window>,
    controllers: [gtk::EventControllerKey; 2],
}

impl ActionKeyHandler {
    /// Attaches a handler to `window`, which keeps it until `uninstall`.
    pub fn install(window: &Window) -> Self {
        let capture = key_controller(window, gtk::PropagationPhase::Capture, Focus::Window);
        let bubble = key_controller(window, gtk::PropagationPhase::Bubble, Focus::Widget);
        Self { window: window.downgrade(), controllers: [capture, bubble] }
    }

    /// Detaches the handler from its window.
    pub fn uninstall(&self) {
        if let Some(window) = self.window.upgrade() {
            for controller in &self.controllers {
                window.remove_controller(controller);
            }
        }
    }
}

/// A key controller on `window` that handles key presses in `phase` while the focus is `lane`.
fn key_controller(window: &Window, phase: gtk::PropagationPhase, lane: Focus) -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(phase);
    let weak = window.downgrade();
    controller.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
        Some(window) => handle(&window, key, state, lane),
        None => glib::Propagation::Proceed,
    });
    window.add_controller(controller.clone());
    controller
}

/// Who a key press belongs to, decided by where the keyboard focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// Nothing that takes text or arrow keys has the focus, so the bindings take a key before the focused widget.
    Window,
    /// A text field or a popover has the focus. It takes its keys first, and the bindings act only on the rest.
    Widget,
    /// A dialog is open, and it has every key.
    Dialog,
}

/// The focus state of `window` right now.
fn focus_of(window: &Window) -> Focus {
    if window.visible_dialog().is_some() {
        return Focus::Dialog;
    }
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) else { return Focus::Window };
    let text = focus.is::<gtk::TextView>() || focus.dynamic_cast_ref::<gtk::Editable>().is_some();
    let in_popover = focus.ancestor(gtk::Popover::static_type()).is_some();
    if text || in_popover { Focus::Widget } else { Focus::Window }
}

/// The chords that open GTK's inspector: Ctrl+Shift+I, and Ctrl+Shift+D.
const GTK_INSPECTOR_CHORDS: &[&str] = &["ctrl+shift+i", "ctrl+shift+d"];

/// Runs the action bound to the key press, if the focus state lets this controller's lane take it. `lane` is the focus
/// state the controller serves: the capture controller serves `Focus::Window`, the bubble one `Focus::Widget`. A key
/// the focused widget did not use reaches the bubble controller. Keys with no binding are swallowed by the window lane
/// and passed on by the widget lane.
fn handle(window: &Window, key: gdk::Key, state: gdk::ModifierType, lane: Focus) -> glib::Propagation {
    if focus_of(window) != lane {
        return glib::Propagation::Proceed;
    }
    // A plain key that a text field or popover leaves is still the widget's: a letter or an arrow key is not a command
    // there. Only a chord with Ctrl, Alt or Super falls through to the bindings.
    if lane == Focus::Widget && is_plain_key(state) {
        return glib::Propagation::Proceed;
    }
    let Some(chord) = chord_from_event(key, state) else { return glib::Propagation::Proceed };
    // GTK's inspector is always reached, whatever the bindings say, so it stays available for debugging.
    if GTK_INSPECTOR_CHORDS.contains(&chord.format().as_str()) {
        return glib::Propagation::Proceed;
    }
    // The crop editor takes these itself: Escape cancels it rather than closing the viewer, Enter applies it, and Tab
    // moves between its edge boxes.
    if lane == Focus::Window
        && window.viewer_is_crop_editing()
        && matches!(key, gdk::Key::Escape | gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab | gdk::Key::ISO_Left_Tab)
    {
        return glib::Propagation::Proceed;
    }
    let scopes = window.active_scopes();
    let Some((action, value, target, script, notification)) = keybindings::with_table(|table| {
        table
            .resolve_binding(&chord.format(), &scopes)
            .map(|(action, binding)| (action, binding.value, binding.target.clone(), binding.script.clone(), binding.notification.clone()))
    }) else {
        return match lane {
            Focus::Window => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        };
    };
    if action.takes_notification() {
        match notification {
            Some(notification) => {
                window.show_notification(&notification.message, &notification.level, notification.seconds);
            }
            None => eprintln!("rsimgv: the binding for {} has no notification", chord.format()),
        }
        return glib::Propagation::Stop;
    }
    if action.takes_script() {
        match script {
            Some(name) => window.run_script(&name),
            None => eprintln!("rsimgv: the binding for {} names no script", chord.format()),
        }
        return glib::Propagation::Stop;
    }
    if window.run_action_with(action.name, value, target.as_deref()) {
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

/// Whether a key press has no Ctrl, Alt or Super: the keys a text field types, and a popover's arrows and letters.
pub fn is_plain_key(state: gdk::ModifierType) -> bool {
    !state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK)
}

const MODIFIER_KEYS: &[&str] = &[
    "Shift_L", "Shift_R", "Control_L", "Control_R", "Alt_L", "Alt_R", "Super_L", "Super_R", "Meta_L", "Meta_R",
    "ISO_Level3_Shift", "Caps_Lock",
];
