//! The overlay's page in the preferences: a map of the item with its nine places, showing the elements stacked in each,
//! a button in each place to add one, and a form for the element picked. Every change is saved at once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::media_overlay::layout_from_settings;
use crate::overlay::template::Template;
use crate::overlay::{
    Element, Items, Kind, Layout, Slot, Visibility, BAR_WIDTH_RANGE, FONT_SIZE_RANGE, PADDING_RANGE, PROPERTIES, RADIUS_RANGE, SECONDS_RANGE,
    STATUS_PARTS, TIME_FORMATS,
};

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
        "Elements in one place stack, the first on top, and their text lines up toward the nearest edge. Add a seek \
         display, the playback status, the notification messages, or text of your own from a template.",
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

    fn add(self: &Rc<Self>, kind: Kind, slot: Slot) {
        let element = Element::of_kind(kind, slot);
        let index = {
            let mut layout = self.layout.borrow_mut();
            layout.elements.push(element);
            layout.elements.len() - 1
        };
        self.save();
        self.select(Some(index));
    }

    /// Moves the element picked up or down among those in its place, which is up or down the stack.
    fn shift(self: &Rc<Self>, down: bool) {
        let Some(index) = self.selected.get() else { return };
        let mut layout = self.layout.borrow_mut();
        let slot = layout.elements[index].slot;
        let neighbour = if down {
            (index + 1..layout.elements.len()).find(|&other| layout.elements[other].slot == slot)
        } else {
            (0..index).rev().find(|&other| layout.elements[other].slot == slot)
        };
        let Some(neighbour) = neighbour else { return };
        layout.elements.swap(index, neighbour);
        drop(layout);
        self.save();
        self.select(Some(neighbour));
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
            let add = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text("Add an element here").valign(gtk::Align::End).vexpand(true).build();
            add.add_css_class("flat");
            let choices = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let popover = gtk::Popover::new();
            for kind in Kind::ALL {
                let choice = gtk::Button::with_label(kind.title());
                choice.add_css_class("flat");
                choice.connect_clicked({
                    let editor = self.clone();
                    let popover = popover.clone();
                    move |_| {
                        popover.popdown();
                        editor.add(kind, slot);
                    }
                });
                choices.append(&choice);
            }
            popover.set_child(Some(&choices));
            add.set_popover(Some(&popover));
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
        group.set_title(element.kind.title());

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

        let order = adw::ActionRow::builder().title("Order").subtitle("Where it is among the elements in its place").build();
        for (icon, down) in [("go-up-symbolic", false), ("go-down-symbolic", true)] {
            let button = gtk::Button::from_icon_name(icon);
            button.set_valign(gtk::Align::Center);
            button.add_css_class("flat");
            button.connect_clicked({
                let editor = self.clone();
                move |_| editor.shift(down)
            });
            order.add_suffix(&button);
        }
        group.add(&order);
        self.form.append(&group);

        match element.kind {
            Kind::Template => self.template_options(&element),
            Kind::Seek => self.seek_options(&element),
            Kind::Status => self.status_options(&element),
            Kind::Messages => {
                let note = adw::PreferencesGroup::new();
                note.set_description(Some("Notification messages appear in this place, and look as set below. Messages that ask for a place of their own still go there."));
                self.form.append(&note);
            }
        }
        if element.kind != Kind::Messages {
            self.form.append(&self.when_options(&element));
        }
        self.form.append(&self.look_options(&element));

        let delete = gtk::Button::with_label("Delete element");
        delete.set_halign(gtk::Align::Start);
        delete.add_css_class("destructive-action");
        delete.connect_clicked({
            let editor = self.clone();
            move |_| editor.remove()
        });
        self.form.append(&delete);
    }

    /// A row that shows the text the element picked makes, for sample values, kept up to date by calling what it returns.
    fn preview_row(self: &Rc<Self>) -> (adw::ActionRow, Rc<dyn Fn()>) {
        let row = adw::ActionRow::builder().title("Preview").build();
        let label = gtk::Label::new(None);
        label.set_selectable(true);
        label.set_wrap(true);
        row.add_suffix(&label);
        let editor = self.clone();
        let update: Rc<dyn Fn()> = Rc::new(move || {
            let template = editor.selected.get().and_then(|index| editor.layout.borrow().elements.get(index).map(Element::text_template));
            label.set_text(&template.map(|template| Template::parse(&template).render(&sample)).unwrap_or_default());
        });
        update();
        (row, update)
    }

    fn template_options(self: &Rc<Self>, element: &Element) {
        let group = adw::PreferencesGroup::new();
        group.set_title("Text");
        let template = adw::EntryRow::builder().title("Text template").text(element.template.as_str()).build();
        let (preview, update) = self.preview_row();
        template.connect_changed({
            let editor = self.clone();
            move |row| {
                let text = row.text().to_string();
                editor.edit(|element| element.template = text);
                update();
            }
        });
        group.add(&template);
        group.add(&preview);
        group.add(&reference());
        self.form.append(&group);
    }

    fn seek_options(self: &Rc<Self>, element: &Element) {
        let group = adw::PreferencesGroup::new();
        group.set_title("Seek display");
        group.set_description(Some("A progress bar and the time, shown for a moment after the video is skipped through."));
        let bar = adw::SwitchRow::builder().title("Progress bar").active(element.bar).build();
        let width = adw::SpinRow::with_range(f64::from(BAR_WIDTH_RANGE.0), f64::from(BAR_WIDTH_RANGE.1), 20.0);
        width.set_title("Bar width");
        width.set_subtitle("In pixels");
        width.set_value(f64::from(element.bar_width));
        width.set_sensitive(element.bar);
        bar.connect_active_notify({
            let editor = self.clone();
            let width = width.clone();
            move |row| {
                width.set_sensitive(row.is_active());
                editor.edit(|element| element.bar = row.is_active());
            }
        });
        width.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.bar_width = row.value() as i32)
        });
        let labels: Vec<&str> = TIME_FORMATS.iter().map(|(_, label)| *label).collect();
        let format = adw::ComboRow::builder().title("Time").subtitle("How the time is written").model(&gtk::StringList::new(&labels)).build();
        format.set_selected(TIME_FORMATS.iter().position(|(name, _)| *name == element.time_format).unwrap_or(0) as u32);
        let (preview, update) = self.preview_row();
        format.connect_selected_notify({
            let editor = self.clone();
            move |row| {
                if let Some((name, _)) = TIME_FORMATS.get(row.selected() as usize) {
                    editor.edit(|element| element.time_format = (*name).to_owned());
                    update();
                }
            }
        });
        group.add(&bar);
        group.add(&width);
        group.add(&format);
        group.add(&preview);
        self.form.append(&group);
    }

    fn status_options(self: &Rc<Self>, element: &Element) {
        let group = adw::PreferencesGroup::new();
        group.set_title("Media status");
        group.set_description(Some("The playback state and time, as mpv writes them on its own display."));
        let (preview, update) = self.preview_row();
        for (part, title, _) in STATUS_PARTS {
            let row = adw::SwitchRow::builder().title(*title).active(element.parts.iter().any(|shown| shown == part)).build();
            row.connect_active_notify({
                let editor = self.clone();
                let update = update.clone();
                move |row| {
                    editor.edit(|element| {
                        element.parts.retain(|shown| shown != part);
                        if row.is_active() {
                            element.parts.push((*part).to_owned());
                        }
                    });
                    update();
                }
            });
            group.add(&row);
        }
        group.add(&preview);
        self.form.append(&group);
    }

    /// When the element is up, and over what.
    fn when_options(self: &Rc<Self>, element: &Element) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        group.set_title("When");
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
        group
    }

    /// The font, colours, corners and blur.
    fn look_options(self: &Rc<Self>, element: &Element) -> adw::PreferencesGroup {
        let style = &element.style;
        let group = adw::PreferencesGroup::new();
        group.set_title("Look");

        let spin = |title: &str, subtitle: &str, range: (f64, f64), step: f64, value: f64| {
            let row = adw::SpinRow::with_range(range.0, range.1, step);
            row.set_title(title);
            row.set_subtitle(subtitle);
            row.set_value(value);
            row
        };
        let font_size = spin("Font size", "In points", FONT_SIZE_RANGE, 1.0, style.font_size);
        font_size.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.font_size = row.value())
        });
        group.add(&font_size);

        let text_color = self.color_row("Text colour", &style.text_color, |style, color| style.text_color = color);
        group.add(&text_color);
        let background = self.color_row("Background colour", &style.background, |style, color| style.background = color);
        group.add(&background);

        let blur = adw::SwitchRow::builder()
            .title("Blur behind")
            .subtitle("Blurs the picture behind it. Needs KDE Plasma on Wayland, and the KDE effects setting")
            .active(style.blur)
            .build();
        blur.connect_active_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.blur = row.is_active())
        });
        group.add(&blur);

        let radius = spin("Corner radius", "In pixels", (f64::from(RADIUS_RANGE.0), f64::from(RADIUS_RANGE.1)), 1.0, f64::from(style.radius));
        radius.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.radius = row.value() as i32)
        });
        group.add(&radius);
        let padding = spin("Padding", "In pixels", (f64::from(PADDING_RANGE.0), f64::from(PADDING_RANGE.1)), 1.0, f64::from(style.padding));
        padding.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.padding = row.value() as i32)
        });
        group.add(&padding);
        let opacity = spin("Opacity", "In percent", (10.0, 100.0), 5.0, style.opacity * 100.0);
        opacity.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.opacity = row.value() / 100.0)
        });
        group.add(&opacity);
        group
    }

    /// A row with a button that picks a colour, with its transparency, and calls `set` with the colour picked.
    fn color_row(self: &Rc<Self>, title: &str, current: &str, set: fn(&mut crate::overlay::Style, String)) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title(title).build();
        let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(true).build()));
        button.set_valign(gtk::Align::Center);
        button.set_rgba(&gtk::gdk::RGBA::parse(current).unwrap_or(gtk::gdk::RGBA::BLACK));
        button.connect_rgba_notify({
            let editor = self.clone();
            move |button| {
                let color = button.rgba().to_str().to_string();
                editor.edit(|element| set(&mut element.style, color));
            }
        });
        row.add_suffix(&button);
        row
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
