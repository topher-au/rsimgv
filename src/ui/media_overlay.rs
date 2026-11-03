//! The media overlay: the elements of the layout (see `crate::overlay`) drawn over the viewer's item. Each place on the
//! item has a column, and an element is a revealer in the column of its place, so it fades in and out as it comes and
//! goes. The overlay only shows things, so the item under it takes the clicks.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::{gdk, gio};

use crate::overlay::template::Template;
use crate::overlay::{DEFAULT_MARGIN, plain_icon, sample, Element, FilenameWrap, IconStyle, Items, Kind, Layout, Legacy, Slot, Style, TextAlign, Visibility};

/// The space between one element and the next, in pixels.
const SPACING: i32 = 6;
/// How long an element takes to fade in and out, in milliseconds.
const FADE: u32 = 150;

/// The layout the settings say: the one saved by the configuration page, or the older settings' while none is saved.
pub fn layout_from_settings(settings: &gio::Settings) -> Layout {
    let saved = settings.string("overlay-layout");
    if let Some(layout) = Layout::from_json(&saved) {
        return layout;
    }
    Layout::from_legacy(&Legacy {
        seek_enabled: settings.boolean("seek-overlay"),
        seek_bar: settings.boolean("seek-overlay-bar"),
        seek_time: settings.boolean("seek-overlay-time"),
        seek_time_format: settings.string("seek-overlay-time-format").to_string(),
        seek_position: settings.string("seek-overlay-position").to_string(),
        seek_seconds: settings.double("seek-overlay-seconds"),
        seek_width: settings.int("seek-overlay-width"),
        seek_always: settings.boolean("seek-overlay-always"),
        status_enabled: settings.boolean("viewer-status-bar"),
        status_position: settings.string("viewer-status-overlay-position").to_string(),
    })
}

/// Keeps the column of the place `slot` `margin` pixels from the edges of the frame that it is at. A place in the middle
/// of a side is not at that edge.
pub fn set_edge_margins(column: &gtk::Box, slot: Slot, margin: i32) {
    column.set_margin_top(if slot.row == 0 { margin } else { 0 });
    column.set_margin_bottom(if slot.row == 2 { margin } else { 0 });
    column.set_margin_start(if slot.column == 0 { margin } else { 0 });
    column.set_margin_end(if slot.column == 2 { margin } else { 0 });
}

/// How the time `micros` into a video is written: `1:23`, or `1:02:05` from an hour in.
pub fn clock(micros: i64) -> String {
    let total = micros.max(0) / 1_000_000;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// How far through the video `position` is, from 0 to 1, or 0 while the duration is unknown.
pub fn fraction(position: i64, duration: i64) -> f64 {
    if duration <= 0 {
        return 0.0;
    }
    (position as f64 / duration as f64).clamp(0.0, 1.0)
}

/// The CSS that gives what matches `selector` the look of `style`. A colour that does not parse is left out, so nothing
/// the settings hold reaches the stylesheet as it was written.
pub fn style_css(selector: &str, style: &Style) -> String {
    // The colour as the settings write it, made as see-through as its opacity says.
    let color = |text: &str, opacity: f64| {
        gdk::RGBA::parse(text).ok().map(|color| {
            let alpha = (color.alpha() * opacity as f32).clamp(0.0, 1.0);
            gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha).to_str().to_string()
        })
    };
    let mut css = format!(
        "{selector} {{ font-size: {}pt; padding: {}px; border-radius: {}px; background-color: {};",
        style.font_size,
        style.padding,
        style.radius,
        color(&style.background, style.background_opacity).unwrap_or_else(|| "transparent".to_owned()),
    );
    if let Some(color) = color(&style.text_color, style.text_opacity) {
        css.push_str(&format!(" color: {color};"));
    }
    // The family is quoted, with what would end the quotes taken out, as it is the user's text.
    let family = style.font_family.replace(['"', '\\', ';', '{', '}'], "");
    if !family.is_empty() {
        css.push_str(&format!(" font-family: \"{family}\";"));
    }
    css.push_str(" }");
    css
}

/// A stylesheet added to the display for as long as it is held.
pub struct Sheet(gtk::CssProvider);

impl Sheet {
    pub fn new(css: &str) -> Self {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(css);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        }
        Self(provider)
    }
}

impl Drop for Sheet {
    fn drop(&mut self) {
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.0);
        }
    }
}

/// A name for a class that only one element's stylesheet matches.
pub fn unique_class(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// How text and the things that hold it line up in `align`: the label's alignment and the holder's.
pub fn align_label(label: &gtk::Label, align: TextAlign) {
    let (xalign, justify) = match align {
        TextAlign::Left => (0.0, gtk::Justification::Left),
        TextAlign::Center => (0.5, gtk::Justification::Center),
        TextAlign::Right => (1.0, gtk::Justification::Right),
    };
    label.set_xalign(xalign);
    label.set_justify(justify);
}

/// Where a place holds the things in it, along the row: against the left edge, the middle, or the right edge.
pub fn horizontal_align(align: TextAlign) -> gtk::Align {
    match align {
        TextAlign::Left => gtk::Align::Start,
        TextAlign::Center => gtk::Align::Center,
        TextAlign::Right => gtk::Align::End,
    }
}

/// An element being edited, shown over the item as it is now, whether or not it is saved. Every other element is faded.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    /// Its index in the saved layout, or `None` for an element that is new.
    pub index: Option<usize>,
    pub element: Element,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
    static WATCHERS: RefCell<Vec<Rc<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
}

/// Shows `preview` over the item, or ends the preview. Whoever watches for it is told.
pub fn set_preview(preview: Option<Preview>) {
    let changed = PREVIEW.with(|current| {
        let changed = *current.borrow() != preview;
        *current.borrow_mut() = preview;
        changed
    });
    if changed {
        let watchers: Vec<Rc<dyn Fn()>> = WATCHERS.with(|watchers| watchers.borrow().clone());
        for watcher in watchers {
            watcher();
        }
    }
}

pub fn preview() -> Option<Preview> {
    PREVIEW.with(|current| current.borrow().clone())
}

/// Calls `callback` whenever the preview starts, changes or ends.
pub fn watch_preview(callback: impl Fn() + 'static) {
    WATCHERS.with(|watchers| watchers.borrow_mut().push(Rc::new(callback)));
}

/// How much of its own opacity an element keeps while another is being edited.
const FADED: f64 = 0.2;

/// What the overlay is drawn from.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    /// The value of each property a template reads, or `None` while it has none.
    pub values: &'a dyn Fn(&str) -> Option<String>,
    /// How far through the video it is, for a progress element's bar.
    pub fraction: f64,
    pub is_video: bool,
    pub playing: bool,
    pub fullscreen: bool,
    /// How long ago the video was skipped through, if it has been since it was shown.
    pub since_seek: Option<Duration>,
}

impl Element {
    /// Whether the element is up for `frame`, before its text is known.
    fn is_up(&self, frame: &Frame) -> bool {
        let over = match self.items {
            Items::All => true,
            Items::Videos => frame.is_video,
            Items::Pictures => !frame.is_video,
        };
        let when = match self.visibility {
            Visibility::Always => true,
            Visibility::Seek => frame.since_seek.is_some_and(|since| since.as_secs_f64() < self.seconds),
            Visibility::Paused => frame.is_video && !frame.playing,
        };
        self.enabled && over && when && (!self.fullscreen_only || frame.fullscreen)
    }
}

/// An element that is on the item.
struct Placed {
    /// Its index in the layout.
    index: usize,
    element: Element,
    template: Template,
    revealer: gtk::Revealer,
    label: gtk::Label,
    bar: Option<gtk::ProgressBar>,
    /// A media status's state icon, and the filename under its text.
    icon: Option<gtk::Label>,
    filename: Option<gtk::Label>,
    filename_template: Template,
    /// Keeps the element's look on the display.
    _sheet: Sheet,
}

/// The widgets of the overlay, added to the viewer's `gtk::Overlay`.
pub struct MediaOverlay {
    columns: Vec<(Slot, gtk::Box)>,
    placed: RefCell<Vec<Placed>>,
    /// The element being edited, built from the preview, and the preview it was built from.
    previewed: RefCell<Option<(Preview, Placed)>>,
    layout: RefCell<Layout>,
}

impl MediaOverlay {
    /// The overlay, over what `overlay` holds, with nothing in it.
    pub fn new(overlay: &gtk::Overlay) -> Self {
        let columns = Slot::all()
            .map(|slot| {
                let column = gtk::Box::new(gtk::Orientation::Vertical, SPACING);
                column.set_halign([gtk::Align::Start, gtk::Align::Center, gtk::Align::End][usize::from(slot.column)]);
                column.set_valign([gtk::Align::Start, gtk::Align::Center, gtk::Align::End][usize::from(slot.row)]);
                set_edge_margins(&column, slot, DEFAULT_MARGIN);
                column.set_can_target(false);
                overlay.add_overlay(&column);
                (slot, column)
            })
            .collect();
        Self { columns, placed: RefCell::new(Vec::new()), previewed: RefCell::new(None), layout: RefCell::new(Layout::default()) }
    }

    /// Whether the overlay is set to a layout, so it has this one already.
    pub fn has_layout(&self, layout: &Layout) -> bool {
        *self.layout.borrow() == *layout
    }

    /// Whether any element is on, which is whether it needs updating as the video plays.
    pub fn any_enabled(&self) -> bool {
        self.layout.borrow().any_enabled()
    }

    /// Replaces the elements with those of `layout`, all hidden until the next update.
    pub fn set_layout(&self, layout: &Layout) {
        self.drop_preview();
        for (slot, column) in &self.columns {
            set_edge_margins(column, *slot, layout.margin(*slot));
        }
        for placed in self.placed.borrow_mut().drain(..) {
            if let Some(column) = placed.revealer.parent().and_downcast::<gtk::Box>() {
                column.remove(&placed.revealer);
            }
        }
        let mut placed = self.placed.borrow_mut();
        // Messages are shown by the window's message overlay, not over the item.
        for (index, element) in layout.elements.iter().enumerate().filter(|(_, element)| element.enabled && element.kind != Kind::Messages) {
            let Some((_, column)) = self.columns.iter().find(|(slot, _)| *slot == element.slot) else { continue };
            let item = build(index, element);
            column.append(&item.revealer);
            placed.push(item);
        }
        self.layout.replace(layout.clone());
    }

    /// Brings each element up or down for `frame`, and writes the text of those that are up. While an element is being
    /// edited it is shown whatever its settings say, with sample values where the item has none, and the others fade.
    pub fn update(&self, frame: &Frame) {
        let preview = preview();
        self.sync_preview(preview.as_ref());
        for item in self.placed.borrow().iter() {
            let edited = preview.as_ref().is_some_and(|preview| preview.index == Some(item.index));
            let faded = if preview.is_some() { FADED } else { 1.0 };
            item.revealer.set_opacity(item.element.style.opacity * faded);
            // The saved version of the element being edited gives way to the preview of it.
            let up = !edited && item.element.is_up(frame) && fill(item, frame);
            if item.revealer.reveals_child() != up {
                item.revealer.set_reveal_child(up);
            }
        }
        if let Some((_, item)) = self.previewed.borrow().as_ref() {
            let sampled = |name: &str| (frame.values)(name).or_else(|| sample(name));
            let shown = Frame { values: &sampled, fraction: frame.fraction.max(0.28), ..*frame };
            fill(item, &shown);
            if !item.revealer.reveals_child() {
                item.revealer.set_reveal_child(true);
            }
        }
    }

    /// Makes the element built for the preview the one being previewed, or takes it away when there is none.
    fn sync_preview(&self, preview: Option<&Preview>) {
        let Some(preview) = preview else {
            self.drop_preview();
            return;
        };
        if self.previewed.borrow().as_ref().is_some_and(|(built, _)| built == preview) {
            return;
        }
        self.drop_preview();
        let Some((_, column)) = self.columns.iter().find(|(slot, _)| *slot == preview.element.slot) else { return };
        let item = build(preview.index.unwrap_or(usize::MAX), &preview.element);
        // It appears at once, and in the stack where the saved element is, if it is in the same place.
        item.revealer.set_transition_duration(0);
        let saved = self.placed.borrow().iter().find(|placed| Some(placed.index) == preview.index && placed.element.slot == preview.element.slot).map(|placed| placed.revealer.clone());
        match saved {
            Some(saved) => column.insert_child_after(&item.revealer, Some(&saved)),
            None => column.append(&item.revealer),
        }
        self.previewed.replace(Some((preview.clone(), item)));
    }

    fn drop_preview(&self) {
        if let Some((_, item)) = self.previewed.take() {
            if let Some(column) = item.revealer.parent().and_downcast::<gtk::Box>() {
                column.remove(&item.revealer);
            }
        }
    }

    /// The elements that are up and want KDE's blur behind them.
    pub fn blurred(&self) -> Vec<gtk::Widget> {
        self.placed
            .borrow()
            .iter()
            .filter(|item| item.element.style.blur && item.revealer.is_child_revealed())
            .map(|item| item.revealer.clone().upcast())
            .collect()
    }

    /// Takes every element down now, as the item they were over is gone.
    pub fn hide_all(&self) {
        self.drop_preview();
        for item in self.placed.borrow().iter() {
            item.revealer.set_reveal_child(false);
        }
    }
}

/// Writes `frame`'s text, icon, filename and bar into the element, and returns whether there is anything of it to show.
fn fill(item: &Placed, frame: &Frame) -> bool {
    let text = item.template.render(frame.values);
    let state_icon = item.icon.as_ref().and_then(|_| (frame.values)("state-icon"));
    if let (Some(icon), Some(text)) = (&item.icon, &state_icon) {
        match item.element.icon_style {
            IconStyle::Emoji => icon.set_text(text),
            IconStyle::Characters => icon.set_text(&plain_icon(text)),
        }
    }
    if let Some(label) = &item.filename {
        label.set_text(&item.filename_template.render(frame.values));
    }
    if item.label.text() != text {
        item.label.set_text(&text);
    }
    item.label.set_visible(!text.is_empty());
    if let Some(bar) = &item.bar {
        bar.set_fraction(frame.fraction);
    }
    // An element with nothing to show does not show its background either.
    item.bar.is_some() || !text.trim().is_empty() || state_icon.is_some() || item.filename.as_ref().is_some_and(|label| !label.text().is_empty())
}

/// How `label`, a filename, fits the room it has: on one line in full, over several lines, or cut short with an ellipsis
/// once it is more than `chars` characters wide.
fn fit_filename(label: &gtk::Label, wrap: FilenameWrap, chars: i32) {
    let (ellipsize, wrapped, width) = match wrap {
        FilenameWrap::SingleLine => (gtk::pango::EllipsizeMode::None, false, -1),
        FilenameWrap::Wrap => (gtk::pango::EllipsizeMode::None, true, chars),
        FilenameWrap::EllipsizeStart => (gtk::pango::EllipsizeMode::Start, false, chars),
        FilenameWrap::EllipsizeMiddle => (gtk::pango::EllipsizeMode::Middle, false, chars),
        FilenameWrap::EllipsizeEnd => (gtk::pango::EllipsizeMode::End, false, chars),
    };
    label.set_ellipsize(ellipsize);
    label.set_wrap(wrapped);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(width);
}

/// The CSS for the filename line of a media status, which is its own size, colour and opacity where the element sets them.
fn filename_css(selector: &str, element: &Element) -> String {
    let mut css = format!("{selector} {{ opacity: {};", element.filename_opacity);
    if element.filename_font_size > 0.0 {
        css.push_str(&format!(" font-size: {}pt;", element.filename_font_size));
    }
    if let Ok(color) = gdk::RGBA::parse(&element.filename_color) {
        let alpha = (color.alpha() * element.style.text_opacity as f32).clamp(0.0, 1.0);
        css.push_str(&format!(" color: {};", gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha).to_str()));
    }
    css.push_str(" }");
    css
}

fn build(index: usize, element: &Element) -> Placed {
    let class = unique_class("overlay-el");
    let align = element.slot.text_align();
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("overlay-element");
    card.add_css_class(&class);
    let bar = (element.kind == Kind::Seek && element.bar).then(|| {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("seek-bar");
        bar.set_size_request(element.bar_width, -1);
        card.append(&bar);
        bar
    });
    let label = gtk::Label::new(None);
    label.add_css_class("overlay-text");
    align_label(&label, align);

    // A media status has an icon beside its text, and the filename under the text, lined up with it.
    let is_status = element.kind == Kind::Status;
    let icon = (is_status && element.shows_part("state")).then(|| {
        let icon = gtk::Label::new(None);
        icon.add_css_class("overlay-text");
        icon.add_css_class("overlay-icon");
        icon.set_valign(gtk::Align::Start);
        icon
    });
    let filename = (is_status && element.shows_part("filename")).then(|| {
        let filename = gtk::Label::new(None);
        filename.add_css_class("overlay-text");
        filename.add_css_class("overlay-subtext");
        fit_filename(&filename, element.filename_wrap, element.filename_chars);
        align_label(&filename, align);
        filename
    });
    if is_status {
        let grid = gtk::Grid::builder().column_spacing(8).row_spacing(2).build();
        let column = i32::from(icon.is_some());
        if let Some(icon) = &icon {
            grid.attach(icon, 0, 0, 1, 1);
        }
        label.set_hexpand(true);
        grid.attach(&label, column, 0, 1, 1);
        if let Some(filename) = &filename {
            filename.set_hexpand(true);
            grid.attach(filename, column, 1, 1, 1);
        }
        card.append(&grid);
    } else {
        card.append(&label);
    }

    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    revealer.set_transition_duration(FADE);
    revealer.set_child(Some(&card));
    revealer.set_opacity(element.style.opacity);
    revealer.set_halign(horizontal_align(align));
    revealer.set_can_target(false);
    let sheet = Sheet::new(&format!(
        "{} {}",
        style_css(&format!(".overlay-element.{class}"), &element.style),
        filename_css(&format!(".overlay-element.{class} .overlay-subtext"), element),
    ));
    Placed {
        index,
        element: element.clone(),
        template: Template::parse(&element.text_template()),
        revealer,
        label,
        bar,
        icon,
        filename,
        filename_template: Template::parse(&element.filename_template),
        _sheet: sheet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000;

    fn frame<'a>(values: &'a dyn Fn(&str) -> Option<String>) -> Frame<'a> {
        Frame { values, fraction: 0.0, is_video: true, playing: true, fullscreen: false, since_seek: None }
    }

    #[test]
    fn the_clock_reads_minutes_and_from_an_hour_hours() {
        assert_eq!(clock(83 * SECOND), "1:23");
        assert_eq!(clock(3_725 * SECOND), "1:02:05");
        assert_eq!(clock(-SECOND), "0:00");
    }

    #[test]
    fn a_position_past_the_ends_is_held_within_the_video() {
        assert_eq!(fraction(400 * SECOND, 296 * SECOND), 1.0);
        assert_eq!(fraction(-SECOND, 296 * SECOND), 0.0);
        assert_eq!(fraction(148 * SECOND, 296 * SECOND), 0.5);
        assert_eq!(fraction(5 * SECOND, 0), 0.0);
    }

    #[test]
    fn an_element_is_up_when_everything_it_asks_for_holds() {
        let none = |_: &str| None;
        let mut element = Element::new("Speed", Slot::TOP_RIGHT, "${speed}");
        assert!(element.is_up(&frame(&none)));

        element.items = Items::Pictures;
        assert!(!element.is_up(&frame(&none)));
        element.items = Items::Videos;

        element.visibility = Visibility::Paused;
        assert!(!element.is_up(&frame(&none)));
        assert!(element.is_up(&Frame { playing: false, ..frame(&none) }));

        element.visibility = Visibility::Seek;
        assert!(!element.is_up(&frame(&none)));
        assert!(element.is_up(&Frame { since_seek: Some(Duration::from_secs(1)), ..frame(&none) }));
        assert!(!element.is_up(&Frame { since_seek: Some(Duration::from_secs(3)), ..frame(&none) }));

        element.visibility = Visibility::Always;
        element.fullscreen_only = true;
        assert!(!element.is_up(&frame(&none)));
        assert!(element.is_up(&Frame { fullscreen: true, ..frame(&none) }));

        element.enabled = false;
        assert!(!element.is_up(&Frame { fullscreen: true, ..frame(&none) }));
    }
}
