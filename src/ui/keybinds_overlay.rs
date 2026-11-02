//! An overlay that lists the keybindings in force where the user is: for each context that applies, each action with
//! its keys. It sits in a corner of the window, clicks go through it, and it moves to another corner when the pointer
//! goes over it, so it never covers what the pointer is at. It is built when it is shown, and again if the context
//! changes while it is up.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use super::keybind_label::KeybindLabel;
use crate::keybindings::actions::{value_info, ActionInfo, Scope};
use crate::keybindings::{self, Binding};

/// The space between the list and the edges of the window, in pixels.
const MARGIN: f64 = 12.0;

/// A corner of the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn alignment(self) -> (gtk::Align, gtk::Align) {
        match self {
            Self::TopLeft => (gtk::Align::Start, gtk::Align::Start),
            Self::TopRight => (gtk::Align::End, gtk::Align::Start),
            Self::BottomLeft => (gtk::Align::Start, gtk::Align::End),
            Self::BottomRight => (gtk::Align::End, gtk::Align::End),
        }
    }

    /// The corner across the window, then the one above or below, then the opposite one: the order to try them in
    /// when the list has to move out of the pointer's way.
    fn others(self) -> [Corner; 3] {
        match self {
            Self::TopLeft => [Self::TopRight, Self::BottomLeft, Self::BottomRight],
            Self::TopRight => [Self::TopLeft, Self::BottomRight, Self::BottomLeft],
            Self::BottomLeft => [Self::BottomRight, Self::TopLeft, Self::TopRight],
            Self::BottomRight => [Self::BottomLeft, Self::TopRight, Self::TopLeft],
        }
    }
}

/// Where a list `size` wide and tall sits in `corner` of a window `area` wide and tall: x, y, width and height.
fn rect_at(corner: Corner, area: (f64, f64), size: (f64, f64)) -> (f64, f64, f64, f64) {
    let (horizontal, vertical) = corner.alignment();
    let x = if horizontal == gtk::Align::Start { MARGIN } else { area.0 - size.0 - MARGIN };
    let y = if vertical == gtk::Align::Start { MARGIN } else { area.1 - size.1 - MARGIN };
    (x, y, size.0, size.1)
}

fn contains(rect: (f64, f64, f64, f64), point: (f64, f64)) -> bool {
    point.0 >= rect.0 && point.0 < rect.0 + rect.2 && point.1 >= rect.1 && point.1 < rect.1 + rect.3
}

/// The corner the list moves to when the pointer is at `pointer` over it in `corner`: the first of the others that
/// the pointer is not over. The list stays where it is if the pointer is not over it, or is over it everywhere.
fn corner_away(corner: Corner, area: (f64, f64), size: (f64, f64), pointer: (f64, f64)) -> Corner {
    if !contains(rect_at(corner, area, size), pointer) {
        return corner;
    }
    corner.others().into_iter().find(|other| !contains(rect_at(*other, area, size), pointer)).unwrap_or(corner)
}

/// The list over a window, or over the picture of the viewer, whichever it is shown over.
pub struct KeybindsOverlay {
    revealer: gtk::Revealer,
    sections: gtk::Box,
    corner: Rc<Cell<Corner>>,
    /// The overlay the list is over, and the controller that follows the pointer over it.
    host: RefCell<Option<(gtk::glib::WeakRef<gtk::Overlay>, gtk::EventControllerMotion)>>,
}

impl KeybindsOverlay {
    /// The overlay, over what `overlay` holds, hidden.
    pub fn new(overlay: &gtk::Overlay) -> Self {
        let sections = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        sections.add_css_class("keybinds-overlay");

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(150);
        revealer.set_child(Some(&sections));
        // Clicks go through to what is under the list.
        revealer.set_can_target(false);
        revealer.set_margin_start(MARGIN as i32);
        revealer.set_margin_end(MARGIN as i32);
        revealer.set_margin_top(MARGIN as i32);
        revealer.set_margin_bottom(MARGIN as i32);
        let corner = Rc::new(Cell::new(Corner::default()));
        place(&revealer, corner.get());
        let list = Self { revealer, sections, corner, host: RefCell::new(None) };
        list.move_to(overlay);
        list
    }

    /// Puts the list over `overlay`, moving it off the one it was over. Its size is then that of `overlay`, so it stays
    /// within what `overlay` covers.
    pub fn move_to(&self, overlay: &gtk::Overlay) {
        if self.host.borrow().as_ref().and_then(|(weak, _)| weak.upgrade()).as_ref() == Some(overlay) {
            return;
        }
        if let Some((weak, motion)) = self.host.borrow_mut().take() {
            if let Some(old) = weak.upgrade() {
                old.remove_controller(&motion);
                old.remove_overlay(&self.revealer);
            }
        }
        overlay.add_overlay(&self.revealer);

        // The list takes no events, so the pointer is followed on the overlay under it.
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        motion.connect_motion({
            let (revealer, corner, host) = (self.revealer.clone(), self.corner.clone(), overlay.downgrade());
            move |_, x, y| {
                let Some(overlay) = host.upgrade().filter(|_| revealer.reveals_child()) else { return };
                let area = (f64::from(overlay.width()), f64::from(overlay.height()));
                let size = (f64::from(revealer.width()), f64::from(revealer.height()));
                let moved = corner_away(corner.get(), area, size, (x, y));
                if moved != corner.get() {
                    corner.set(moved);
                    place(&revealer, moved);
                }
            }
        });
        overlay.add_controller(motion.clone());
        *self.host.borrow_mut() = Some((overlay.downgrade(), motion));
    }

    pub fn is_shown(&self) -> bool {
        self.revealer.reveals_child()
    }

    pub fn hide(&self) {
        self.revealer.set_reveal_child(false);
    }

    /// Lists the keybindings in force in `active` and the global context, and shows the list.
    pub fn show(&self, active: &[Scope]) {
        self.rebuild(active);
        self.revealer.set_reveal_child(true);
    }

    /// Makes the list the keybindings in force in `active`, for a context that changed while it is up.
    pub fn rebuild(&self, active: &[Scope]) {
        while let Some(child) = self.sections.first_child() {
            self.sections.remove(&child);
        }
        let rows = keybindings::with_table(|table| {
            table
                .active_bindings(active)
                .into_iter()
                .map(|(action, binding)| (action, binding.clone()))
                .collect::<Vec<(&'static ActionInfo, Binding)>>()
        });
        // The deepest context first, the one most of what is wanted is in, and Global last.
        let mut scopes: Vec<Scope> = Scope::ALL.into_iter().filter(|scope| *scope == Scope::Global || active.contains(scope)).collect();
        scopes.sort_by_key(|scope| std::cmp::Reverse(scope.depth()));
        for scope in scopes {
            let in_scope: Vec<&(&ActionInfo, Binding)> = rows.iter().filter(|(_, binding)| binding.context == scope).collect();
            if !in_scope.is_empty() {
                self.sections.append(&section(scope, &in_scope));
            }
        }
        if self.sections.first_child().is_none() {
            let none = gtk::Label::new(Some("Nothing is bound here."));
            none.add_css_class("dim-label");
            self.sections.append(&none);
        }
    }
}

/// Puts the list in `corner`.
fn place(revealer: &gtk::Revealer, corner: Corner) {
    let (horizontal, vertical) = corner.alignment();
    revealer.set_halign(horizontal);
    revealer.set_valign(vertical);
}

/// One context's actions, each with the keys bound to it there.
fn section(scope: Scope, rows: &[&(&'static ActionInfo, Binding)]) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    section.set_valign(gtk::Align::Start);
    let heading = gtk::Label::new(Some(scope.title()));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    section.append(&heading);
    let grid = gtk::Grid::new();
    grid.set_column_spacing(16);
    grid.set_row_spacing(4);
    // An action with several keys in a context is one row, with each of its keys.
    let mut shown: Vec<(&'static ActionInfo, String, Vec<&Binding>)> = Vec::new();
    for (action, binding) in rows.iter().map(|row| (row.0, &row.1)) {
        let label = row_label(action, binding);
        match shown.iter_mut().find(|(other, other_label, _)| other.name == action.name && *other_label == label) {
            Some((_, _, keys)) => keys.push(binding),
            None => shown.push((action, label, vec![binding])),
        }
    }
    for (row, (_, label, keys)) in shown.iter().enumerate() {
        let name = gtk::Label::new(Some(label));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        grid.attach(&name, 0, row as i32, 1, 1);
        let chords = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for binding in keys {
            chords.append(&KeybindLabel::new(&binding.chord));
        }
        chords.set_halign(gtk::Align::End);
        grid.attach(&chords, 1, row as i32, 1, 1);
    }
    section.append(&grid);
    section
}

/// An action's name for the list, with what its binding passes after it: a value, a script or a message.
fn row_label(action: &ActionInfo, binding: &Binding) -> String {
    if let Some(info) = value_info(action.name) {
        return format!("{} ({})", action.label, info.format(binding.value.unwrap_or(info.default)));
    }
    if let Some(script) = &binding.script {
        return format!("{} ({script})", action.label);
    }
    if let Some(notification) = &binding.notification {
        return format!("{} ({})", action.label, notification.message);
    }
    action.label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: (f64, f64) = (1000.0, 800.0);
    const SIZE: (f64, f64) = (300.0, 200.0);

    #[test]
    fn the_list_sits_in_its_corner() {
        assert_eq!(rect_at(Corner::TopLeft, AREA, SIZE), (12.0, 12.0, 300.0, 200.0));
        assert_eq!(rect_at(Corner::TopRight, AREA, SIZE), (688.0, 12.0, 300.0, 200.0));
        assert_eq!(rect_at(Corner::BottomLeft, AREA, SIZE), (12.0, 588.0, 300.0, 200.0));
        assert_eq!(rect_at(Corner::BottomRight, AREA, SIZE), (688.0, 588.0, 300.0, 200.0));
    }

    #[test]
    fn the_list_stays_while_the_pointer_is_elsewhere() {
        assert_eq!(corner_away(Corner::TopRight, AREA, SIZE, (100.0, 400.0)), Corner::TopRight);
    }

    #[test]
    fn the_list_moves_across_when_the_pointer_goes_over_it() {
        assert_eq!(corner_away(Corner::TopRight, AREA, SIZE, (800.0, 50.0)), Corner::TopLeft);
        assert_eq!(corner_away(Corner::BottomLeft, AREA, SIZE, (50.0, 700.0)), Corner::BottomRight);
    }

    #[test]
    fn in_a_narrow_window_it_goes_where_the_pointer_is_not() {
        // Across the top is no better here, as the two corners overlap at the pointer.
        let narrow = (500.0, 800.0);
        assert_eq!(corner_away(Corner::TopRight, narrow, SIZE, (250.0, 50.0)), Corner::BottomRight);
        // And in a window the list fills, there is nowhere to go.
        assert_eq!(corner_away(Corner::TopRight, (300.0, 200.0), SIZE, (100.0, 100.0)), Corner::TopRight);
    }
}
