//! An overlay that lists the keybindings in force where the user is: for each context that applies, each action with
//! its keys. It is built when it is shown, and again if the context changes while it is up.

use gtk::prelude::*;

use super::keybind_label::KeybindLabel;
use crate::keybindings::actions::{value_info, ActionInfo, Scope};
use crate::keybindings::{self, Binding};

/// The most of the window's height the list takes before it scrolls, as a share.
const MAX_HEIGHT_SHARE: f64 = 0.7;
/// The width of the list, in pixels.
const WIDTH: i32 = 560;

/// The panel over the window.
pub struct KeybindsOverlay {
    revealer: gtk::Revealer,
    title: gtk::Label,
    sections: gtk::Box,
    scroller: gtk::ScrolledWindow,
}

impl KeybindsOverlay {
    /// The overlay, over what `overlay` holds, hidden.
    pub fn new(overlay: &gtk::Overlay) -> Self {
        let title = gtk::Label::new(None);
        title.add_css_class("title-3");
        title.set_xalign(0.0);
        let sections = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&sections));
        scroller.set_propagate_natural_height(true);
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
        panel.add_css_class("keybinds-overlay");
        panel.set_size_request(WIDTH, -1);
        panel.append(&title);
        panel.append(&scroller);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(150);
        revealer.set_child(Some(&panel));
        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::Center);
        revealer.set_margin_start(24);
        revealer.set_margin_end(24);
        revealer.set_margin_top(24);
        revealer.set_margin_bottom(24);
        overlay.add_overlay(&revealer);
        Self { revealer, title, sections, scroller }
    }

    pub fn is_shown(&self) -> bool {
        self.revealer.reveals_child()
    }

    pub fn hide(&self) {
        self.revealer.set_reveal_child(false);
    }

    /// Lists the keybindings in force in `active` and the global context, and shows the list. `height` is the height of
    /// the window, which the list stays within.
    pub fn show(&self, active: &[Scope], height: i32) {
        self.rebuild(active);
        self.scroller.set_max_content_height(((f64::from(height) * MAX_HEIGHT_SHARE) as i32).max(160));
        self.revealer.set_reveal_child(true);
    }

    /// Makes the list the keybindings in force in `active`, for a context that changed while it is up.
    pub fn rebuild(&self, active: &[Scope]) {
        let title = active.iter().map(|scope| scope.title()).collect::<Vec<_>>().join(", ");
        self.title.set_label(&format!("Keybindings: {title}"));
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
            if in_scope.is_empty() {
                continue;
            }
            self.sections.append(&section(scope, &in_scope));
        }
        if self.sections.first_child().is_none() {
            let none = gtk::Label::new(Some("Nothing is bound here."));
            none.add_css_class("dim-label");
            self.sections.append(&none);
        }
    }
}

/// One context's actions, each with the keys bound to it there.
fn section(scope: Scope, rows: &[&(&'static ActionInfo, Binding)]) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
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
