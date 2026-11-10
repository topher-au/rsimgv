//! Menu items that show the shortcut bound to their action. An item's shortcut is GTK's `accel` attribute, which only
//! draws the label: the keys themselves are the key handler's, so an item never fires twice. Menus built on the spot take
//! the bindings in force when they are built. Menus that live as long as the window are tracked, and rebuilt when the
//! bindings change.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::keybindings::{self, chord::Chord};

/// Menu actions whose binding lives under another name. The sort menu's Show Hidden Files item is the browse view's
/// Toggle Hidden Files binding, and the main menu's Preferences runs the same action as Settings.
const BOUND_AS: &[(&str, &str)] = &[("win.show-hidden", "browse.toggle-hidden"), ("app.preferences", "win.settings")];

/// Key names that GDK spells with capitals, which the chords hold in lowercase.
const KEY_NAMES: &[(&str, &str)] = &[("backspace", "BackSpace"), ("iso_left_tab", "ISO_Left_Tab"), ("kp_enter", "KP_Enter")];

thread_local! {
    /// The static menus that are rebuilt when the bindings change.
    static TRACKED: RefCell<Vec<glib::WeakRef<gio::Menu>>> = const { RefCell::new(Vec::new()) };
}

/// A menu item that runs `action` and shows the shortcut bound to it.
pub fn item(label: &str, action: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    show_shortcut(&item, action);
    item
}

/// A menu item that runs `action` with `target` and shows the shortcut bound to it.
pub fn targeted(label: &str, action: &str, target: &glib::Variant) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(action), Some(target));
    show_shortcut(&item, action);
    item
}

/// Makes `menu`, which is built once and kept, show the shortcuts bound now, and keeps it up to date with them: see
/// `refresh_tracked`.
pub fn track(menu: &gio::Menu) {
    TRACKED.with(|tracked| tracked.borrow_mut().push(menu.downgrade()));
    refresh(menu);
}

/// Rebuilds every tracked menu with the shortcuts bound now. Call it after the bindings change.
pub fn refresh_tracked() {
    let menus: Vec<gio::Menu> = TRACKED.with(|tracked| {
        let mut tracked = tracked.borrow_mut();
        tracked.retain(|menu| menu.upgrade().is_some());
        tracked.iter().filter_map(glib::WeakRef::upgrade).collect()
    });
    for menu in &menus {
        refresh(menu);
    }
}

/// Sets the shortcut `item` shows for `action`, if the action is bound.
fn show_shortcut(item: &gio::MenuItem, action: &str) {
    if let Some(accel) = accelerator(action) {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
}

/// Rebuilds the items of `menu` that run an action, so they show the shortcut bound now, and does the same for the
/// sections and submenus in it. The items are replaced in place, which keeps their order.
fn refresh(menu: &gio::Menu) {
    for position in 0..menu.n_items() {
        for link in ["section", "submenu"] {
            if let Some(inner) = menu.item_link(position, link).and_then(|model| model.downcast::<gio::Menu>().ok()) {
                refresh(&inner);
            }
        }
        let text = |attribute: &str| menu.item_attribute_value(position, attribute, None).and_then(|value| value.get::<String>());
        let Some(action) = text("action") else { continue };
        if menu.item_link(position, "section").is_some() || menu.item_link(position, "submenu").is_some() {
            continue;
        }
        let label = text("label").unwrap_or_default();
        let fresh = match menu.item_attribute_value(position, "target", None) {
            Some(target) => targeted(&label, &action, &target),
            None => item(&label, &action),
        };
        menu.remove(position);
        menu.insert_item(position, &fresh);
    }
}

/// The GTK accelerator text for the shortcut bound to `action`, such as `<Control><Shift>p`, if it is bound to one
/// GTK can name.
fn accelerator(action: &str) -> Option<String> {
    let name = BOUND_AS.iter().find(|(menu, _)| *menu == action).map_or(action, |(_, bound)| bound);
    let chord = Chord::parse(&keybindings::shortcut(name)?).ok()?;
    let mut modifiers = gdk::ModifierType::empty();
    for (on, modifier) in [
        (chord.ctrl, gdk::ModifierType::CONTROL_MASK),
        (chord.alt, gdk::ModifierType::ALT_MASK),
        (chord.shift, gdk::ModifierType::SHIFT_MASK),
        (chord.super_key, gdk::ModifierType::SUPER_MASK),
    ] {
        if on {
            modifiers |= modifier;
        }
    }
    let key = key_named(&chord.key)?;
    Some(gtk::accelerator_name(key, modifiers).to_string())
}

/// The GDK key with the chord's key name. Chords hold lowercase names, so the names GDK spells with capitals are tried
/// as well.
fn key_named(name: &str) -> Option<gdk::Key> {
    let spelled = KEY_NAMES.iter().find(|(lower, _)| *lower == name).map_or_else(|| capitalized(name), |(_, key)| (*key).to_owned());
    gdk::Key::from_name(name).or_else(|| gdk::Key::from_name(&spelled))
}

/// `page_down` as GDK spells it: `Page_Down`.
fn capitalized(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("_")
}
