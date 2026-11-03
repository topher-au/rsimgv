//! The overlay's page in the preferences. It shows a map of the item with its nine places and the elements stacked in
//! each, which can be dragged to another place or to another spot in their own. Clicking an element, or the + in a
//! place, swaps the map for the element's editor: tabs for each group of options, and Save and Cancel under them. The
//! changes to the element are kept apart until it is saved.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::media_overlay::{layout_from_settings, set_preview, Preview};
use crate::overlay::template::Template;
use crate::overlay::{
    sample, Element, IconStyle, Items, Kind, Layout, Overflow, Slot, Style, Visibility, BAR_WIDTH_RANGE, FONT_SIZE_RANGE, MAX_NOTIFICATIONS_RANGE, PADDING_RANGE, PROPERTIES, RADIUS_RANGE,
    SECONDS_RANGE, STATUS_PARTS, TIME_FORMATS,
};

/// The element being edited, and where it goes back to.
struct Draft {
    /// Its index in the layout, or `None` for an element that is new.
    index: Option<usize>,
    element: Element,
}

struct Editor {
    settings: gio::Settings,
    layout: RefCell<Layout>,
    draft: RefCell<Option<Draft>>,
    /// Holds the map and the element's editor, one at a time.
    pages: gtk::Stack,
    map: gtk::Grid,
    /// The element's editor: its tabs, and the switcher for them.
    editor_view: adw::ToolbarView,
    switcher: adw::ViewSwitcher,
    delete: gtk::Button,
}

const MAP_PAGE: &str = "map";
const EDITOR_PAGE: &str = "editor";

/// The page, to put in the preferences.
pub fn build(settings: &gio::Settings) -> gtk::Widget {
    let editor = Rc::new(Editor {
        settings: settings.clone(),
        layout: RefCell::new(layout_from_settings(settings)),
        draft: RefCell::new(None),
        pages: gtk::Stack::builder().vexpand(true).hexpand(true).build(),
        map: gtk::Grid::builder().row_homogeneous(true).column_homogeneous(true).row_spacing(6).column_spacing(6).build(),
        editor_view: adw::ToolbarView::new(),
        switcher: adw::ViewSwitcher::builder().policy(adw::ViewSwitcherPolicy::Wide).build(),
        delete: gtk::Button::with_label("Delete"),
    });
    editor.map.add_css_class("overlay-map");
    editor.map.set_size_request(-1, 300);

    // The map, with what it is for above it and the reset below.
    let map_page = adw::PreferencesPage::new();
    let layout_group = adw::PreferencesGroup::new();
    layout_group.set_description(Some(
        "Drag elements between places, or within one, to arrange them. Elements in one place stack, the first on top, and \
         their text lines up toward the nearest edge. Click one to change it, or add one with +.",
    ));
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&editor.map));
    layout_group.add(&frame);
    map_page.add(&layout_group);
    let reset_group = adw::PreferencesGroup::new();
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
    reset_group.add(&reset);
    map_page.add(&reset_group);
    super::preferences::tighten(map_page.upcast_ref());
    editor.pages.add_named(&map_page, Some(MAP_PAGE));

    // The element's editor: tabs on top, Save and Cancel at the bottom.
    editor.editor_view.add_top_bar(&editor.switcher);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_margin_top(6);
    buttons.set_margin_bottom(6);
    buttons.set_margin_start(12);
    buttons.set_margin_end(12);
    editor.delete.add_css_class("destructive-action");
    editor.delete.connect_clicked({
        let editor = editor.clone();
        move |_| editor.delete()
    });
    buttons.append(&editor.delete);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    buttons.append(&spacer);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.connect_clicked({
        let editor = editor.clone();
        move |_| editor.close_editor()
    });
    buttons.append(&cancel);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    save.connect_clicked({
        let editor = editor.clone();
        move |_| editor.save_draft()
    });
    buttons.append(&save);
    editor.editor_view.add_bottom_bar(&buttons);
    editor.pages.add_named(&editor.editor_view, Some(EDITOR_PAGE));

    editor.refresh_map();

    // The preview is up only while the editor is on screen: leaving the page or closing the dialog ends it.
    editor.pages.connect_unmap(|_| set_preview(None));
    editor.pages.connect_map({
        let weak = Rc::downgrade(&editor);
        move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.publish_preview();
            }
        }
    });

    // A change to the setting from outside, such as an edit of the settings file, is shown.
    let weak = Rc::downgrade(&editor);
    settings.connect_changed(Some("overlay-layout"), move |_, _| {
        if let Some(editor) = weak.upgrade() {
            if layout_from_settings(&editor.settings) != *editor.layout.borrow() {
                editor.reload();
            }
        }
    });
    editor.pages.clone().upcast()
}

impl Editor {
    fn reload(self: &Rc<Self>) {
        self.layout.replace(layout_from_settings(&self.settings));
        self.close_editor();
    }

    fn save(&self) {
        let _ = self.settings.set_string("overlay-layout", &self.layout.borrow().to_json());
    }

    /// Changes the element being edited with `change`. Nothing is saved until the editor is.
    fn edit(&self, change: impl FnOnce(&mut Element)) {
        if let Some(draft) = self.draft.borrow_mut().as_mut() {
            change(&mut draft.element);
        }
        self.publish_preview();
    }

    /// Shows the element being edited over the item, as it is now, with the other elements faded.
    fn publish_preview(&self) {
        set_preview(self.draft.borrow().as_ref().map(|draft| Preview { index: draft.index, element: draft.element.clone() }));
    }

    /// Opens the editor on the element at `index`.
    fn open(self: &Rc<Self>, index: usize) {
        let element = self.layout.borrow().elements.get(index).cloned();
        if let Some(element) = element {
            self.open_draft(Draft { index: Some(index), element });
        }
    }

    /// Opens the editor on a new element of `kind` in `slot`, which is added to the layout when it is saved.
    fn open_new(self: &Rc<Self>, kind: Kind, slot: Slot) {
        self.open_draft(Draft { index: None, element: Element::of_kind(kind, slot) });
    }

    fn open_draft(self: &Rc<Self>, draft: Draft) {
        let element = draft.element.clone();
        self.delete.set_visible(draft.index.is_some());
        self.draft.replace(Some(draft));

        let tabs = adw::ViewStack::new();
        tabs.add_titled_with_icon(&self.general_tab(&element), Some("general"), "General", "preferences-system-symbolic");
        match element.kind {
            Kind::Template => {
                tabs.add_titled_with_icon(&self.template_tab(&element), Some("content"), "Text", "format-text-bold-symbolic");
            }
            Kind::Seek => {
                tabs.add_titled_with_icon(&self.seek_tab(&element), Some("content"), "Seek", "media-seek-forward-symbolic");
            }
            Kind::Status => {
                tabs.add_titled_with_icon(&self.status_tab(&element), Some("content"), "Status", "media-playback-start-symbolic");
            }
            Kind::Messages => {}
        }
        tabs.add_titled_with_icon(&self.look_tab(&element), Some("look"), "Look", "applications-graphics-symbolic");
        for page in tabs.pages().iter::<adw::ViewStackPage>().flatten() {
            super::preferences::tighten(&page.child());
        }
        self.switcher.set_stack(Some(&tabs));
        self.editor_view.set_content(Some(&tabs));
        self.pages.set_visible_child_name(EDITOR_PAGE);
        self.publish_preview();
    }

    /// Goes back to the map, dropping what was changed in the editor.
    fn close_editor(self: &Rc<Self>) {
        self.draft.replace(None);
        set_preview(None);
        self.refresh_map();
        self.pages.set_visible_child_name(MAP_PAGE);
    }

    fn save_draft(self: &Rc<Self>) {
        if let Some(Draft { index, element }) = self.draft.take() {
            {
                let mut layout = self.layout.borrow_mut();
                match index.filter(|&index| index < layout.elements.len()) {
                    Some(index) => layout.elements[index] = element,
                    None => layout.elements.push(element),
                }
            }
            self.save();
        }
        self.close_editor();
    }

    fn delete(self: &Rc<Self>) {
        if let Some(index) = self.draft.borrow().as_ref().and_then(|draft| draft.index) {
            let mut layout = self.layout.borrow_mut();
            if index < layout.elements.len() {
                layout.elements.remove(index);
            }
            drop(layout);
            self.save();
        }
        self.close_editor();
    }

    /// Moves the element at `from` into `slot`, in front of the element at `before` if there is one, and saves.
    fn move_element(self: &Rc<Self>, from: usize, slot: Slot, before: Option<usize>) {
        if self.layout.borrow_mut().move_element(from, slot, before) {
            self.save();
        }
        // The chip that was dragged is part of what is redrawn, so that waits until the drop is over.
        let editor = self.clone();
        glib::idle_add_local_once(move || editor.refresh_map());
    }

    /// Draws the nine places with the elements in each.
    fn refresh_map(self: &Rc<Self>) {
        while let Some(child) = self.map.first_child() {
            self.map.remove(&child);
        }
        for slot in Slot::all() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
            cell.add_css_class("overlay-map-cell");
            let heading = gtk::Label::new(Some(&slot.name().replace('-', " ")));
            heading.add_css_class("caption");
            heading.add_css_class("dim-label");
            cell.append(&heading);
            for (index, element) in self.layout.borrow().elements.iter().enumerate().filter(|(_, element)| element.slot == slot) {
                let chip = gtk::Button::with_label(&element.name);
                chip.add_css_class("overlay-chip");
                if !element.enabled {
                    chip.add_css_class("dim-label");
                }
                chip.set_tooltip_text(Some(element.kind.title()));
                chip.connect_clicked({
                    let editor = self.clone();
                    move |_| editor.open(index)
                });
                self.drag_source(&chip, index);
                self.drop_target(&chip, slot, Some(index), "drop-before");
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
                        editor.open_new(kind, slot);
                    }
                });
                choices.append(&choice);
            }
            popover.set_child(Some(&choices));
            add.set_popover(Some(&popover));
            cell.append(&add);
            self.drop_target(&cell, slot, None, "drop-hover");
            self.map.attach(&cell, i32::from(slot.column), i32::from(slot.row), 1, 1);
        }
    }

    /// Lets `chip`, the element at `index`, be dragged.
    fn drag_source(&self, chip: &gtk::Button, index: usize) {
        let source = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&(index as u32).to_value())));
        source.connect_drag_begin({
            let chip = chip.clone();
            move |source, _| source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&chip))), 0, 0)
        });
        chip.add_controller(source);
    }

    /// Lets an element dragged from the map be dropped on `widget`, which moves it into `slot` in front of the element
    /// at `before`, or to the end of the place. `hover` is the style class that shows where it would land.
    fn drop_target(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>, slot: Slot, before: Option<usize>, hover: &'static str) {
        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        target.connect_drop({
            let editor = self.clone();
            let widget = widget.clone().upcast::<gtk::Widget>();
            move |_, value, _, _| {
                widget.remove_css_class(hover);
                let Ok(from) = value.get::<u32>() else { return false };
                editor.move_element(from as usize, slot, before);
                true
            }
        });
        target.connect_enter({
            let widget = widget.clone().upcast::<gtk::Widget>();
            move |_, _, _| {
                widget.add_css_class(hover);
                gdk::DragAction::MOVE
            }
        });
        target.connect_leave({
            let widget = widget.clone().upcast::<gtk::Widget>();
            move |_| widget.remove_css_class(hover)
        });
        widget.add_controller(target);
    }

    /// A row that shows the text the element being edited makes, for sample values, kept up to date by calling what it
    /// returns.
    fn preview_row(self: &Rc<Self>) -> (adw::ActionRow, Rc<dyn Fn()>) {
        let row = adw::ActionRow::builder().title("Preview").build();
        let label = gtk::Label::new(None);
        label.set_selectable(true);
        label.set_wrap(true);
        row.add_suffix(&label);
        let editor = self.clone();
        let update: Rc<dyn Fn()> = Rc::new(move || {
            let template = editor.draft.borrow().as_ref().map(|draft| draft.element.text_template());
            label.set_text(&template.map(|template| Template::parse(&template).render(&sample)).unwrap_or_default());
        });
        update();
        (row, update)
    }

    fn general_tab(self: &Rc<Self>, element: &Element) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title(element.kind.title());
        if element.kind == Kind::Messages {
            group.set_description(Some("Notification messages appear in this place, and look as set under Look. A message that asks for a place of its own still goes there."));
        }

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

        if element.kind == Kind::Messages {
            self.add_stack_rows(&group, element);
        } else {
            self.add_when_rows(&group, element);
        }
        page.add(&group);
        page
    }

    fn template_tab(self: &Rc<Self>, element: &Element) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
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
        page.add(&group);

        let help = adw::PreferencesGroup::new();
        help.add(&reference());
        page.add(&help);
        page
    }

    fn seek_tab(self: &Rc<Self>, element: &Element) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
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
        page.add(&group);
        page
    }

    fn status_tab(self: &Rc<Self>, element: &Element) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
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
        let icons = adw::ComboRow::builder()
            .title("State icons")
            .subtitle("How playing, paused and skipping are drawn")
            .model(&gtk::StringList::new(&["Emoji ▶️ ⏸️", "Characters ▶ ⏸"]))
            .build();
        icons.set_selected(u32::from(element.icon_style == IconStyle::Characters));
        icons.connect_selected_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.icon_style = if row.selected() == 1 { IconStyle::Characters } else { IconStyle::Emoji })
        });
        group.add(&icons);
        group.add(&preview);
        page.add(&group);
        page
    }

    /// Adds to `group` the rows for how many notifications stack up, and what happens when the stack is full.
    fn add_stack_rows(self: &Rc<Self>, group: &adw::PreferencesGroup, element: &Element) {
        let most = adw::SpinRow::with_range(f64::from(MAX_NOTIFICATIONS_RANGE.0), f64::from(MAX_NOTIFICATIONS_RANGE.1), 1.0);
        most.set_title("Most shown at once");
        most.set_subtitle("Notifications stack up, the newest nearest the edge, to this many");
        most.set_value(f64::from(element.max_notifications));
        most.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.max_notifications = row.value() as i32)
        });
        group.add(&most);

        let overflow = adw::ComboRow::builder()
            .title("When the stack is full")
            .model(&gtk::StringList::new(&["Push out the oldest", "Wait for room"]))
            .build();
        overflow.set_selected(u32::from(element.overflow == Overflow::Wait));
        overflow.connect_selected_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.overflow = if row.selected() == 1 { Overflow::Wait } else { Overflow::PushOut })
        });
        group.add(&overflow);
    }

    /// Adds to `group` the rows for when the element is up, and over what.
    fn add_when_rows(self: &Rc<Self>, group: &adw::PreferencesGroup, element: &Element) {
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
    }

    /// The font, colours, corners and blur.
    fn look_tab(self: &Rc<Self>, element: &Element) -> adw::PreferencesPage {
        let style = &element.style;
        let page = adw::PreferencesPage::new();
        let text = adw::PreferencesGroup::new();
        text.set_title("Text");
        let boxed = adw::PreferencesGroup::new();
        boxed.set_title("Background");
        let whole = adw::PreferencesGroup::new();
        whole.set_title("Element");

        let spin = |title: &str, subtitle: &str, range: (f64, f64), step: f64, value: f64| {
            let row = adw::SpinRow::with_range(range.0, range.1, step);
            row.set_title(title);
            row.set_subtitle(subtitle);
            row.set_value(value);
            row
        };
        let percent = |title: &str, subtitle: &str, value: f64| spin(title, subtitle, (0.0, 100.0), 5.0, (value * 100.0).round());

        let family = adw::ActionRow::builder().title("Font").subtitle("The family of the font").build();
        let font = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
        font.set_level(gtk::FontLevel::Family);
        font.set_valign(gtk::Align::Center);
        if !style.font_family.is_empty() {
            font.set_font_desc(&gtk::pango::FontDescription::from_string(&style.font_family));
        }
        font.connect_font_desc_notify({
            let editor = self.clone();
            move |button| {
                let family = button.font_desc().and_then(|desc| desc.family()).map(|family| family.to_string()).unwrap_or_default();
                editor.edit(|element| element.style.font_family = family);
            }
        });
        family.add_suffix(&font);
        text.add(&family);
        let font_size = spin("Font size", "In points", FONT_SIZE_RANGE, 1.0, style.font_size);
        font_size.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.font_size = row.value())
        });
        text.add(&font_size);
        text.add(&self.color_row("Text colour", &style.text_color, |style, color| style.text_color = color));
        let text_opacity = percent("Text opacity", "In percent", style.text_opacity);
        text_opacity.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.text_opacity = row.value() / 100.0)
        });
        text.add(&text_opacity);

        boxed.add(&self.color_row("Background colour", &style.background, |style, color| style.background = color));
        let background_opacity = percent("Background opacity", "In percent", style.background_opacity);
        background_opacity.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.background_opacity = row.value() / 100.0)
        });
        boxed.add(&background_opacity);
        let blur = adw::SwitchRow::builder()
            .title("Blur behind")
            .subtitle("Blurs the picture behind it. Needs KDE Plasma on Wayland, and the KDE effects setting")
            .active(style.blur)
            .build();
        blur.connect_active_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.blur = row.is_active())
        });
        boxed.add(&blur);
        let radius = spin("Corner radius", "In pixels", (f64::from(RADIUS_RANGE.0), f64::from(RADIUS_RANGE.1)), 1.0, f64::from(style.radius));
        radius.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.radius = row.value() as i32)
        });
        boxed.add(&radius);
        let padding = spin("Padding", "In pixels", (f64::from(PADDING_RANGE.0), f64::from(PADDING_RANGE.1)), 1.0, f64::from(style.padding));
        padding.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.padding = row.value() as i32)
        });
        boxed.add(&padding);

        let opacity = spin("Opacity", "Of the whole element, in percent", (10.0, 100.0), 5.0, style.opacity * 100.0);
        opacity.connect_value_notify({
            let editor = self.clone();
            move |row| editor.edit(|element| element.style.opacity = row.value() / 100.0)
        });
        whole.add(&opacity);

        page.add(&text);
        page.add(&boxed);
        page.add(&whole);
        page
    }

    /// A row with a button that picks a colour, and calls `set` with the colour picked.
    fn color_row(self: &Rc<Self>, title: &str, current: &str, set: fn(&mut Style, String)) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title(title).build();
        let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(false).build()));
        button.set_valign(gtk::Align::Center);
        button.set_rgba(&gdk::RGBA::parse(current).unwrap_or(gdk::RGBA::BLACK));
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
