//! The overlay's page in the preferences: a map of the item with its nine places, showing the elements in each, a button
//! in each place to add one, and a form for the element picked. Every change is saved at once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::media_overlay::layout_from_settings;
use crate::overlay::template::Template;
use crate::overlay::{Element, Items, Kind, Layout, Slot, Visibility, BAR_WIDTH_RANGE, PROPERTIES, SCALE_RANGE, SECONDS_RANGE};

/// What a property reads as in the form's preview.
fn sample(name: &str) -> Option<String> {
    let value = match name {
        "state" => "Playing",
        "time-pos" => "1:23",
        "duration" => "4:56",
        "time-remaining" => "3:33",
        "percent-pos" => "28",
        "speed" => "1.5",
        "volume" => "80",
        "fps" => "29.97",
        "filename" => "example.mp4",
        "path" => "/home/user/Videos/example.mp4",
        "playlist-pos" => "3",
        "playlist-count" => "12",
        "resolution" => "1920 × 1080",
        "file-size" => "48.2 MB",
        _ => return None,
    };
    Some(value.to_owned())
}

struct Editor {
    settings: gio::Settings,
    layout: RefCell<Layout>,
    /// The index in the layout of the element the form edits.
    selected: Cell<Option<usize>>,
    map: gtk::Grid,
    form: gtk::Box,
}

/// The editor, to put in a preferences page.
pub fn build(settings: &gio::Settings) -> gtk::Widget {
    let editor = Rc::new(Editor {
        settings: settings.clone(),
        layout: RefCell::new(layout_from_settings(settings)),
        selected: Cell::new(None),
        map: gtk::Grid::builder().row_homogeneous(true).column_homogeneous(true).row_spacing(6).column_spacing(6).build(),
        form: gtk::Box::new(gtk::Orientation::Vertical, 12),
    });
    editor.map.add_css_class("overlay-map");
    editor.map.set_size_request(-1, 300);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let intro = gtk::Label::new(Some(
        "Pick where each element sits over the item. Elements are text made from a template, or a progress bar with text; \
         each can show always, for a moment after skipping, or while paused.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    root.append(&intro);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&editor.map));
    root.append(&frame);
    root.append(&editor.form);

    let reset = gtk::Button::with_label("Reset the overlay");
    reset.set_halign(gtk::Align::End);
    reset.add_css_class("destructive-action");
    reset.set_tooltip_text(Some("Goes back to the layout the older seek overlay and status bar settings make"));
    reset.connect_clicked({
        let editor = editor.clone();
        move |_| {
            editor.settings.reset("overlay-layout");
            editor.reload();
        }
    });
    root.append(&reset);

    editor.refresh_map();
    editor.refresh_form();

    // A change to the setting from outside, such as an edit of the settings file, is shown.
    let weak = Rc::downgrade(&editor);
    settings.connect_changed(Some("overlay-layout"), move |_, _| {
        if let Some(editor) = weak.upgrade() {
            if layout_from_settings(&editor.settings) != *editor.layout.borrow() {
                editor.reload();
            }
        }
    });
    root.upcast()
}

impl Editor {
    fn reload(self: &Rc<Self>) {
        self.layout.replace(layout_from_settings(&self.settings));
        self.selected.set(None);
        self.refresh_map();
        self.refresh_form();
    }

    fn save(&self) {
        let _ = self.settings.set_string("overlay-layout", &self.layout.borrow().to_json());
    }

    /// Changes the element picked with `change`, and saves it.
    fn edit(self: &Rc<Self>, change: impl FnOnce(&mut Element)) {
        let Some(index) = self.selected.get() else { return };
        if let Some(element) = self.layout.borrow_mut().elements.get_mut(index) {
            change(element);
        }
        self.save();
        self.refresh_map();
    }

    fn select(self: &Rc<Self>, index: Option<usize>) {
        self.selected.set(index);
        self.refresh_map();
        self.refresh_form();
    }

    fn add(self: &Rc<Self>, slot: Slot) {
        let mut element = Element::new("New element", slot, "${state} ${time-pos}${?duration: / ${duration}}");
        element.items = Items::Videos;
        let index = {
            let mut layout = self.layout.borrow_mut();
            layout.elements.push(element);
            layout.elements.len() - 1
        };
        self.save();
        self.select(Some(index));
    }

    fn remove(self: &Rc<Self>) {
        let Some(index) = self.selected.get() else { return };
        self.layout.borrow_mut().elements.remove(index);
        self.save();
        self.select(None);
    }

    /// Draws the nine places with the elements in each.
    fn refresh_map(self: &Rc<Self>) {
        while let Some(child) = self.map.first_child() {
            self.map.remove(&child);
        }
        for slot in Slot::all() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
            cell.add_css_class("overlay-map-cell");
            cell.set_valign(gtk::Align::Fill);
            let heading = gtk::Label::new(Some(&slot.name().replace('-', " ")));
            heading.add_css_class("caption");
            heading.add_css_class("dim-label");
            cell.append(&heading);
            for (index, element) in self.layout.borrow().elements.iter().enumerate().filter(|(_, element)| element.slot == slot) {
                let chip = gtk::ToggleButton::with_label(&element.name);
                chip.set_active(self.selected.get() == Some(index));
                if !element.enabled {
                    chip.add_css_class("dim-label");
                }
                chip.connect_clicked({
                    let editor = self.clone();
                    move |_| editor.select(Some(index))
                });
                cell.append(&chip);
            }
            let add = gtk::Button::from_icon_name("list-add-symbolic");
            add.add_css_class("flat");
            add.set_tooltip_text(Some("Add an element here"));
            add.set_valign(gtk::Align::End);
            add.set_vexpand(true);
            add.connect_clicked({
                let editor = self.clone();
                move |_| editor.add(slot)
            });
            cell.append(&add);
            self.map.attach(&cell, i32::from(slot.column), i32::from(slot.row), 1, 1);
        }
    }

    /// Draws the form for the element picked, or a note to pick one.
    fn refresh_form(self: &Rc<Self>) {
        while let Some(child) = self.form.first_child() {
            self.form.remove(&child);
        }
        let element = self.selected.get().and_then(|index| self.layout.borrow().elements.get(index).cloned());
        let Some(element) = element else {
            let note = gtk::Label::new(Some("Pick an element above to change it, or add one to a place with +."));
            note.add_css_class("dim-label");
            self.form.append(&note);
            return;
        };

        let group = adw::PreferencesGroup::new();
        group.set_title("Element");

        let name = adw::EntryRow::builder().title("Name").text(element.name.as_str()).build();
        name.connect_changed({
            let editor = self.clone();
            move |row| editor.edit(|element| element.name = row.text().to_string())
        });
        group.add(&name);

        let enabled = adw::SwitchRow::builder().title("Shown").active(element.enabled).build();
        enabled.connect_active_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.enabled = row.is_active())
        });
        group.add(&enabled);

        let position = adw::ActionRow::builder().title("Position").subtitle("Where it sits over the item").build();
        position.add_suffix(&self.slot_picker(element.slot));
        group.add(&position);

        let kind = adw::ComboRow::builder().title("Type").model(&gtk::StringList::new(&["Text", "Progress bar with text"])).build();
        kind.set_selected(u32::from(element.kind == Kind::Progress));
        kind.connect_selected_notify({
            let editor = self.clone();
            move |row| {
                editor.edit(|element| element.kind = if row.selected() == 1 { Kind::Progress } else { Kind::Text });
                editor.refresh_form();
            }
        });
        group.add(&kind);

        let template = adw::EntryRow::builder().title("Text template").text(element.template.as_str()).build();
        let preview = adw::ActionRow::builder().title("Preview").build();
        let preview_label = gtk::Label::new(None);
        preview_label.set_selectable(true);
        preview.add_suffix(&preview_label);
        let show_preview = {
            let preview_label = preview_label.clone();
            move |source: &str| preview_label.set_text(&Template::parse(source).render(&sample))
        };
        show_preview(&element.template);
        template.connect_changed({
            let editor = self.clone();
            move |row| {
                let text = row.text().to_string();
                show_preview(&text);
                editor.edit(|element| element.template = text);
            }
        });
        group.add(&template);
        group.add(&preview);
        group.add(&reference());

        let visibility = adw::ComboRow::builder()
            .title("Show")
            .model(&gtk::StringList::new(&["Always", "After skipping", "While paused"]))
            .build();
        visibility.set_selected(match element.visibility {
            Visibility::Always => 0,
            Visibility::Seek => 1,
            Visibility::Paused => 2,
        });
        let seconds = adw::SpinRow::with_range(SECONDS_RANGE.0, SECONDS_RANGE.1, 0.5);
        seconds.set_title("Time shown");
        seconds.set_subtitle("How long it stays up after a skip, in seconds");
        seconds.set_digits(1);
        seconds.set_value(element.seconds);
        seconds.set_sensitive(element.visibility == Visibility::Seek);
        visibility.connect_selected_notify({
            let editor = self.clone();
            let seconds = seconds.clone();
            move |row| {
                let visibility = match row.selected() {
                    1 => Visibility::Seek,
                    2 => Visibility::Paused,
                    _ => Visibility::Always,
                };
                seconds.set_sensitive(visibility == Visibility::Seek);
                editor.edit(|element| element.visibility = visibility);
            }
        });
        seconds.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.seconds = row.value())
        });
        group.add(&visibility);
        group.add(&seconds);

        let items = adw::ComboRow::builder().title("Show over").model(&gtk::StringList::new(&["Pictures and videos", "Videos", "Pictures"])).build();
        items.set_selected(match element.items {
            Items::All => 0,
            Items::Videos => 1,
            Items::Pictures => 2,
        });
        items.connect_selected_notify({
            let editor = self.clone();
            move |row| {
                let items = match row.selected() {
                    1 => Items::Videos,
                    2 => Items::Pictures,
                    _ => Items::All,
                };
                editor.edit(|element| element.items = items);
            }
        });
        group.add(&items);

        let fullscreen = adw::SwitchRow::builder().title("Only in fullscreen").active(element.fullscreen_only).build();
        fullscreen.connect_active_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.fullscreen_only = row.is_active())
        });
        group.add(&fullscreen);

        let style = adw::PreferencesGroup::new();
        style.set_title("Look");
        let scale = adw::SpinRow::with_range(SCALE_RANGE.0, SCALE_RANGE.1, 0.1);
        scale.set_title("Text size");
        scale.set_subtitle("As a multiple of the usual size");
        scale.set_digits(1);
        scale.set_value(element.scale);
        scale.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.scale = row.value())
        });
        style.add(&scale);
        let opacity = adw::SpinRow::with_range(10.0, 100.0, 5.0);
        opacity.set_title("Opacity");
        opacity.set_subtitle("In percent");
        opacity.set_value(element.opacity * 100.0);
        opacity.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.opacity = row.value() / 100.0)
        });
        style.add(&opacity);
        let card = adw::SwitchRow::builder().title("Card").subtitle("A translucent panel behind it").active(element.card).build();
        card.connect_active_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.card = row.is_active())
        });
        style.add(&card);
        if element.kind == Kind::Progress {
            let width = adw::SpinRow::with_range(f64::from(BAR_WIDTH_RANGE.0), f64::from(BAR_WIDTH_RANGE.1), 20.0);
            width.set_title("Bar width");
            width.set_subtitle("In pixels");
            width.set_value(f64::from(element.bar_width));
            width.connect_value_notify({
                let editor = self.clone();
                move |row| editor.edit(|element| element.bar_width = row.value() as i32)
            });
            style.add(&width);
        }

        let delete = gtk::Button::with_label("Delete element");
        delete.set_halign(gtk::Align::Start);
        delete.add_css_class("destructive-action");
        delete.connect_clicked({
            let editor = self.clone();
            move |_| editor.remove()
        });

        self.form.append(&group);
        self.form.append(&style);
        self.form.append(&delete);
    }

    /// A grid of nine buttons, one for each place, of which the one for `current` is down. Pressing another moves the
    /// element there.
    fn slot_picker(self: &Rc<Self>, current: Slot) -> gtk::Grid {
        let grid = gtk::Grid::builder().row_spacing(2).column_spacing(2).valign(gtk::Align::Center).build();
        let mut first: Option<gtk::ToggleButton> = None;
        for slot in Slot::all() {
            let button = gtk::ToggleButton::new();
            button.set_size_request(28, 20);
            button.set_tooltip_text(Some(&slot.name().replace('-', " ")));
            match &first {
                Some(first) => button.set_group(Some(first)),
                None => first = Some(button.clone()),
            }
            button.set_active(slot == current);
            button.connect_toggled({
                let editor = self.clone();
                move |button| {
                    if button.is_active() {
                        editor.edit(|element| element.slot = slot);
                    }
                }
            });
            grid.attach(&button, i32::from(slot.column), i32::from(slot.row), 1, 1);
        }
        grid
    }
}

/// A row that lists the properties a template can read and how to write them.
fn reference() -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder().title("Template help").subtitle("Properties and syntax").build();
    for (name, about) in PROPERTIES {
        let property = adw::ActionRow::builder().title(format!("${{{name}}}")).subtitle(*about).subtitle_selectable(true).build();
        property.set_title_selectable(true);
        row.add_row(&property);
    }
    let syntax = [
        ("${name:text}", "text while the property has no value"),
        ("${?name:text}", "text only while the property has a value"),
        ("${!name:text}", "text only while the property has no value"),
        ("$$", "a dollar sign"),
    ];
    for (code, about) in syntax {
        row.add_row(&adw::ActionRow::builder().title(code).subtitle(about).build());
    }
    row
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The samples cover every property the page lists.
    #[test]
    fn every_listed_property_has_a_sample_but_mute() {
        for (name, _) in PROPERTIES {
            assert!(sample(name).is_some() || *name == "mute", "{name} has no sample");
        }
    }
}
