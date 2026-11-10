//! The playlist's toolbar: a row of buttons, with the ones that do not fit its width collapsed into a menu at its end. The
//! menu lists them in the order they are in the row, as the buttons they stand for.

use gtk::prelude::*;
use gtk::gio;

use super::menus;
use super::OverflowBox;

/// One of the toolbar's entries, in the order the row shows them.
pub enum Slot {
    /// A button that runs the action `action`. A toggle shows the state of its action.
    Button { label: &'static str, icon: &'static str, action: &'static str, toggle: bool },
    /// A button that opens `menu`. When it is collapsed, it is listed as a submenu.
    Menu { label: &'static str, icon: &'static str, menu: gio::Menu },
    /// Takes the room the buttons leave, so the entries after it sit at the end of the row.
    Spacer,
}

/// The toolbar, which lays its row out again whenever its width changes.
pub struct Toolbar {
    bar: gtk::Box,
}

impl Toolbar {
    /// Makes the toolbar with `slots` in their order, then the overflow menu's button, then `end`, which stays at the end.
    pub fn new(slots: Vec<Slot>, end: &impl IsA<gtk::Widget>) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.set_margin_start(4);
        bar.set_margin_end(4);
        bar.add_css_class("menu");
        let overflow = OverflowBox::new();
        overflow.set_hexpand(true);
        for slot in slots {
            match slot {
                Slot::Button { label, icon, action, toggle } => {
                    let button = if toggle { gtk::ToggleButton::new().upcast::<gtk::Button>() } else { gtk::Button::new() };
                    button.set_icon_name(icon);
                    button.set_tooltip_text(Some(label));
                    button.add_css_class("flat");
                    button.set_action_name(Some(action));
                    overflow.add_button(&button);
                }
                Slot::Menu { label, icon, menu } => {
                    menus::track(&menu);
                    let button = gtk::MenuButton::new();
                    button.set_icon_name(icon);
                    button.set_tooltip_text(Some(label));
                    button.add_css_class("flat");
                    button.set_menu_model(Some(&menu));
                    overflow.add_menu_button(&button);
                }
                Slot::Spacer => overflow.add_spacer(),
            }
        }
        overflow.set_overflow_tooltip("More playlist actions");
        bar.append(&overflow);
        bar.append(end);
        Self { bar }
    }

    /// The toolbar's widget.
    pub fn widget(&self) -> &gtk::Box {
        &self.bar
    }
}
