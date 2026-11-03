//! The playlist's toolbar: a row of buttons, with the ones that do not fit its width collapsed into a menu at its end. The
//! menu lists them in the order they are in the row, as the buttons they stand for.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

/// One of the toolbar's entries, in the order the row shows them.
pub enum Slot {
    /// A button that runs the action `action`. A toggle shows the state of its action.
    Button { label: &'static str, icon: &'static str, action: &'static str, toggle: bool },
    /// A button that opens `menu`. When it is collapsed, it is listed as a submenu.
    Menu { label: &'static str, icon: &'static str, menu: gio::Menu },
    /// Takes the room the buttons leave, so the entries after it sit at the end of the row.
    Spacer,
}

/// A button that may be collapsed, with its width when it was last measured and its entry in the overflow menu.
struct Collapsible {
    widget: gtk::Widget,
    width: Cell<i32>,
    entry: gio::MenuItem,
}

/// What the layout works on: the buttons that may collapse, the overflow menu, and the entry kept at the end of the row.
struct Layout {
    collapsible: Vec<Collapsible>,
    overflow: gtk::MenuButton,
    overflow_menu: gio::Menu,
    end: gtk::Widget,
}

impl Layout {
    /// Shows the buttons that fit in `width`, and lists the rest in the overflow menu, in their order.
    fn fit(&self, width: i32) {
        // Every button is shown while it is measured, so that each has its own width.
        for slot in &self.collapsible {
            slot.widget.set_visible(true);
            slot.width.set(natural_width(&slot.widget));
        }
        self.overflow.set_visible(true);
        let overflow = natural_width(self.overflow.upcast_ref::<gtk::Widget>());
        let end = natural_width(&self.end);
        let count = self.collapsible.len();
        let mut shown = count;
        let mut used: i32 = self.collapsible.iter().map(|slot| slot.width.get()).sum();
        if used + end > width {
            // Something must collapse, and the overflow menu's own button takes room too.
            while shown > 0 && used + overflow + end > width {
                shown -= 1;
                used -= self.collapsible[shown].width.get();
            }
        }
        for slot in &self.collapsible[shown..] {
            slot.widget.set_visible(false);
        }
        self.overflow.set_visible(shown < count);
        self.overflow_menu.remove_all();
        for slot in &self.collapsible[shown..] {
            self.overflow_menu.append_item(&slot.entry);
        }
    }
}

fn natural_width(widget: &gtk::Widget) -> i32 {
    widget.measure(gtk::Orientation::Horizontal, -1).1
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
        let mut collapsible = Vec::new();
        for slot in slots {
            match slot {
                Slot::Button { label, icon, action, toggle } => {
                    let button = if toggle { gtk::ToggleButton::new().upcast::<gtk::Button>() } else { gtk::Button::new() };
                    button.set_icon_name(icon);
                    button.set_tooltip_text(Some(label));
                    button.add_css_class("flat");
                    button.set_action_name(Some(action));
                    bar.append(&button);
                    collapsible.push(Collapsible {
                        widget: button.upcast(),
                        width: Cell::new(0),
                        entry: gio::MenuItem::new(Some(label), Some(action)),
                    });
                }
                Slot::Menu { label, icon, menu } => {
                    let button = gtk::MenuButton::new();
                    button.set_icon_name(icon);
                    button.set_tooltip_text(Some(label));
                    button.add_css_class("flat");
                    button.set_menu_model(Some(&menu));
                    bar.append(&button);
                    collapsible.push(Collapsible {
                        widget: button.upcast(),
                        width: Cell::new(0),
                        entry: gio::MenuItem::new_submenu(Some(label), &menu),
                    });
                }
                Slot::Spacer => {
                    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    spacer.set_hexpand(true);
                    bar.append(&spacer);
                }
            }
        }
        let overflow_menu = gio::Menu::new();
        let overflow = gtk::MenuButton::new();
        overflow.set_icon_name("view-more-symbolic");
        overflow.set_tooltip_text(Some("More playlist actions"));
        overflow.add_css_class("flat");
        overflow.set_menu_model(Some(&overflow_menu));
        bar.append(&overflow);
        let end = end.upcast_ref::<gtk::Widget>().clone();
        bar.append(&end);

        let layout = Rc::new(Layout { collapsible, overflow, overflow_menu, end });
        let laid_out = Cell::new(0);
        bar.add_tick_callback(move |bar, _| {
            let width = bar.width();
            if width > 0 && width != laid_out.get() {
                laid_out.set(width);
                layout.fit(width);
            }
            glib::ControlFlow::Continue
        });
        Self { bar }
    }

    /// The toolbar's widget.
    pub fn widget(&self) -> &gtk::Box {
        &self.bar
    }
}
