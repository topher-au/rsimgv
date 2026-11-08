//! The media overlay: the elements of the layout (see `crate::overlay`) drawn over the viewer's item. Each place on the
//! item has a column, and an element is a revealer in the column of its place, so it fades in and out as it comes and
//! goes. The overlay only shows things, so the item under it takes the clicks.

use crate::settings::Settings;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::gdk;

use super::media_viewer::MAX_VOLUME;
use super::rotated_label::RotatedLabel;

use crate::overlay::template::Template;
use crate::overlay::{ChildAlign, TRIGGERS, DEFAULT_MARGIN, plain_icon, sample, Element, FilenameWrap, IconStyle, Items, Kind, Layout, Legacy, Slot, Style, TextAlign, Visibility};

/// The space between one element and the next, in pixels.
const SPACING: i32 = 6;
/// The width of a media status's state icon, in characters.
const ICON_CHARS: i32 = 2;

/// The layout the settings say: the one saved by the configuration page, or the older settings' while none is saved.
pub fn layout_from_settings(settings: &Settings) -> Layout {
    let saved = settings.string("viewer.overlay-layout");
    if let Some(layout) = Layout::from_json(&saved) {
        return layout;
    }
    Layout::from_legacy(&Legacy {
        seek_enabled: settings.boolean("video.seek-overlay.enabled"),
        seek_bar: settings.boolean("video.seek-overlay.bar"),
        seek_time: settings.boolean("video.seek-overlay.time"),
        seek_time_format: settings.string("video.seek-overlay.time-format").to_string(),
        seek_position: settings.string("video.seek-overlay.position").to_string(),
        seek_seconds: settings.double("video.seek-overlay.seconds"),
        seek_width: settings.int("video.seek-overlay.width"),
        seek_always: settings.boolean("video.seek-overlay.always"),
        status_enabled: settings.boolean("viewer.status-bar.shown"),
        status_position: settings.string("viewer.status-bar.overlay-position").to_string(),
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

/// Makes `revealer` fade in and out as `element` says, or appear and go at once.
pub fn set_fade(revealer: &gtk::Revealer, element: &Element) {
    revealer.set_transition_type(if element.fade { gtk::RevealerTransitionType::Crossfade } else { gtk::RevealerTransitionType::None });
    revealer.set_transition_duration(element.fade_ms.max(0) as u32);
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
        if style.background_on { color(&style.background, style.background_opacity) } else { None }.unwrap_or_else(|| "transparent".to_owned()),
    );
    if let Some(color) = color(&style.text_color, style.text_opacity) {
        css.push_str(&format!(" color: {color};"));
    }
    if style.min_width > 0 {
        css.push_str(&format!(" min-width: {}px;", style.min_width));
    }
    if style.min_height > 0 {
        css.push_str(&format!(" min-height: {}px;", style.min_height));
    }
    // The family is quoted, with what would end the quotes taken out, as it is the user's text.
    let family = style.font_family.replace(['"', '\\', ';', '{', '}'], "");
    if !family.is_empty() {
        css.push_str(&format!(" font-family: \"{family}\";"));
    }
    css.push_str(" }");
    css
}

/// The CSS for the stroke and shadows of `style`: the text of what matches `text_selector` is outlined and shadowed, and
/// what matches `box_selector` is shadowed behind its background. A stroke is made of copies of the letters shadowed round
/// them, as CSS has no stroke for text.
pub fn effects_css(box_selector: &str, text_selector: &str, style: &Style) -> String {
    let text_shadows: Vec<String> = text_shadows(style)
        .into_iter()
        .map(|(dx, dy, blur, color)| format!("{dx:.2}px {dy:.2}px {blur}px {}", color.to_str()))
        .collect();
    let text_shadow = if text_shadows.is_empty() { "none".to_owned() } else { text_shadows.join(", ") };
    // A shadow under a background that is not there would only show round nothing.
    let box_shadow = match (style.shadow_on && style.box_shadow && style.background_on, drop_shadow(style)) {
        (true, Some((dx, dy, blur, color))) => format!("{dx}px {dy}px {blur}px {}", color.to_str()),
        _ => "none".to_owned(),
    };
    format!("{text_selector} {{ text-shadow: {text_shadow}; }} {box_selector} {{ box-shadow: {box_shadow}; }}")
}

/// A colour of `style`, as the settings write it, made as see-through as `opacity` says.
fn faded_color(text: &str, opacity: f64) -> Option<gdk::RGBA> {
    gdk::RGBA::parse(text).ok().map(|color| {
        let alpha = (color.alpha() * opacity as f32).clamp(0.0, 1.0);
        gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha)
    })
}

/// The drop shadow of `style`: how far across and down, how blurred, and its colour.
fn drop_shadow(style: &Style) -> Option<(f32, f32, f32, gdk::RGBA)> {
    faded_color(&style.shadow_color, style.shadow_opacity).map(|color| (style.shadow_x as f32, style.shadow_y as f32, style.shadow_blur as f32, color))
}

/// What is drawn under the letters of `style`, top one first: the stroke, which is made of copies of the letters shadowed
/// round them as there is no stroke for text, then the drop shadow.
pub fn text_shadows(style: &Style) -> Vec<(f32, f32, f32, gdk::RGBA)> {
    let mut shadows = Vec::new();
    if style.stroke_on && style.stroke_width > 0.0 {
        if let Some(stroke) = faded_color(&style.stroke_color, 1.0) {
            // Copies all the way round, closer together the wider the stroke, so it has no gaps.
            let copies = if style.stroke_width > 2.0 { 24 } else { 12 };
            for step in 0..copies {
                let angle = f64::from(step) / f64::from(copies) * std::f64::consts::TAU;
                shadows.push(((angle.cos() * style.stroke_width) as f32, (angle.sin() * style.stroke_width) as f32, 0.0, stroke));
            }
        }
    }
    if style.shadow_on && style.text_shadow {
        shadows.extend(drop_shadow(style));
    }
    shadows
}

/// The text of an element: a label, or for turned text a label that draws it at an angle.
#[derive(Clone)]
enum TextLabel {
    Plain(gtk::Label),
    Turned(RotatedLabel),
}

impl TextLabel {
    fn new(element: &Element) -> Self {
        if element.text_rotation.abs() < f64::EPSILON {
            return Self::Plain(gtk::Label::new(None));
        }
        let label = RotatedLabel::new("", element.text_rotation);
        label.set_shadows(text_shadows(&element.style));
        Self::Turned(label)
    }

    fn widget(&self) -> gtk::Widget {
        match self {
            Self::Plain(label) => label.clone().upcast(),
            Self::Turned(label) => label.clone().upcast(),
        }
    }

    /// Shows `text`, rendered as Pango markup if `markup` is set. Markup that is not valid is shown as it is.
    fn show(&self, text: &str, markup: bool) {
        match self {
            Self::Plain(label) => {
                if markup && gtk::pango::parse_markup(text, '\0').is_ok() {
                    label.set_markup(text);
                } else {
                    label.set_text(text);
                }
            }
            Self::Turned(label) => {
                if markup {
                    label.set_markup(text);
                } else {
                    label.set_text(text);
                }
            }
        }
    }
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

/// How faint a volume display is for a video with no sound.
const DIMMED_OPACITY: f64 = 0.4;

/// The icons that say which of a volume display's bars is the video's and which the application's.
const VIDEO_VOLUME_ICON: &str = "video-x-generic-symbolic";
const APP_VOLUME_ICON: &str = "applications-system-symbolic";

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
    /// The volume level the video's volume bar shows, from 0 to 2, where 1 is full volume and the rest is boost.
    pub volume_fraction: f64,
    /// The application volume, shown as a second bar while each video's volume is tracked.
    pub app_volume_fraction: f64,
    /// Whether each video's volume is tracked, which shows the application volume beside the video's own.
    pub video_volume_tracked: bool,
    /// Whether the video has sound; the volume display is dimmed when it has none.
    pub has_audio: bool,
    /// How long ago each change an element can wait for last happened, if it has since the item was shown.
    pub since_change: [Option<Duration>; TRIGGERS],
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
            Visibility::Seek => frame.since_change[self.kind.trigger() as usize].is_some_and(|since| since.as_secs_f64() < self.seconds),
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
    label: TextLabel,
    /// The text last given to the label, as the template made it.
    shown: RefCell<String>,
    bar: Option<gtk::ProgressBar>,
    /// A volume bar's darker bar under `bar`, which shows the boost above full volume.
    boost_bar: Option<gtk::ProgressBar>,
    /// The application volume's bar, and the darker bar under it, while each video's volume is tracked.
    app_bar: Option<gtk::ProgressBar>,
    app_boost_bar: Option<gtk::ProgressBar>,
    /// The application volume's bar and its icon, which are shown only while each video's volume is tracked.
    app_volume: Option<gtk::Widget>,
    /// The video volume's icon, which is shown only while the application's bar is, so one bar needs no label.
    video_volume_icon: Option<gtk::Widget>,
    /// What goes in the card for the bar: the bar itself, or the two bars of a volume display side by side.
    bar_widget: Option<gtk::Widget>,
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
            let shown = Frame {
                values: &sampled,
                fraction: frame.fraction.max(0.28),
                volume_fraction: frame.volume_fraction.max(0.8),
                app_volume_fraction: frame.app_volume_fraction.max(0.6),
                ..*frame
            };
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
    // A template element can have its text rendered as markup, after the template makes it. What the template reads is
    // escaped then, so a name with an & or a < in it cannot break the markup.
    let markup = item.element.markup && item.element.kind == Kind::Template;
    let text = if markup {
        item.template.render(&|name| (frame.values)(name).map(|value| glib::markup_escape_text(&value).to_string()))
    } else {
        item.template.render(frame.values)
    };
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
    if *item.shown.borrow() != text {
        item.label.show(&text, markup);
        item.shown.replace(text.clone());
    }
    item.label.widget().set_visible(!text.is_empty());
    if let Some(bar) = &item.bar {
        if item.element.kind == Kind::Volume {
            // The bar runs from silence to twice full volume; the part past full volume is the darker bar showing through.
            let level = frame.volume_fraction.clamp(0.0, MAX_VOLUME);
            bar.set_fraction(level.min(1.0) / MAX_VOLUME);
            if let Some(boost) = &item.boost_bar {
                boost.set_fraction(level / MAX_VOLUME);
            }
            if let Some(app) = &item.app_bar {
                // The application volume follows the same scale as the video's, boost and all.
                let level = frame.app_volume_fraction.clamp(0.0, MAX_VOLUME);
                app.set_fraction(level.min(1.0) / MAX_VOLUME);
                if let Some(boost) = &item.app_boost_bar {
                    boost.set_fraction(level / MAX_VOLUME);
                }
            }
            if let Some(app) = &item.app_volume {
                app.set_visible(frame.video_volume_tracked);
            }
            if let Some(icon) = &item.video_volume_icon {
                icon.set_visible(frame.video_volume_tracked);
            }
            if let Some(widget) = &item.bar_widget {
                widget.set_opacity(if frame.has_audio { 1.0 } else { DIMMED_OPACITY });
            }
        } else {
            bar.set_fraction(frame.fraction);
        }
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

/// The video's bar and the application's beside it, each with an icon on the side away from the text. The application's
/// cell and the video's icon start hidden, as they are shown only while each video's volume is tracked.
fn volume_bars(element: &Element, video: gtk::Widget, application: gtk::Widget) -> (gtk::Widget, gtk::Widget, gtk::Widget) {
    // The bars run across the text: side by side beside text above or below, and one above the other beside text at a
    // side. Their icons are on the side away from the text, each against its own bar.
    let beside = element.text_position.is_beside();
    let icon_first = !element.text_position.is_first();
    let icon_size = element.style.font_size.clamp(8.0, 48.0).round() as i32;
    let icon = |name| {
        let icon = gtk::Image::from_icon_name(name);
        icon.set_pixel_size(icon_size);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        icon
    };
    let cell = |bar: gtk::Widget, icon: gtk::Image| {
        let cell = gtk::Box::new(if beside { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical }, element.spacing);
        if icon_first {
            cell.append(&icon);
            cell.append(&bar);
        } else {
            cell.append(&bar);
            cell.append(&icon);
        }
        cell
    };
    let video_icon = icon(VIDEO_VOLUME_ICON);
    let video = cell(video, video_icon.clone());
    let application = cell(application, icon(APP_VOLUME_ICON));
    video_icon.set_visible(false);
    application.set_visible(false);
    let bars = gtk::Box::new(if beside { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, element.spacing);
    bars.append(&video);
    bars.append(&application);
    (bars.upcast(), application.upcast(), video_icon.upcast())
}

fn build(index: usize, element: &Element) -> Placed {
    let class = unique_class("overlay-el");
    let align = element.justify.text_align(element.slot);
    let card = gtk::Box::new(gtk::Orientation::Vertical, element.spacing);
    card.add_css_class("overlay-element");
    card.add_css_class(&class);
    // A bar can stand upright, and its text sits above, below or beside it.
    let beside = element.text_position.is_beside();
    if matches!(element.kind, Kind::Seek | Kind::Volume) && beside {
        card.set_orientation(gtk::Orientation::Horizontal);
    }
    let upright = element.bar_vertical;
    let make_bar = || {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("seek-bar");
        if upright {
            bar.set_orientation(gtk::Orientation::Vertical);
            bar.set_inverted(true);
            bar.set_size_request(-1, element.bar_width);
            bar.set_halign(gtk::Align::Center);
        } else {
            bar.set_size_request(element.bar_width, -1);
            bar.set_valign(gtk::Align::Center);
        }
        bar
    };
    let bar = (matches!(element.kind, Kind::Seek | Kind::Volume) && element.bar).then(make_bar);
    // A volume bar has a second one under it, in a darker colour, which only shows past full volume.
    let boost_bar = (element.kind == Kind::Volume && bar.is_some()).then(|| {
        let boost = make_bar();
        boost.add_css_class("boost-bar");
        boost
    });
    // The application volume has a bar of its own beside the video's, shown while each video's volume is tracked.
    let app_bar = (element.kind == Kind::Volume && bar.is_some()).then(make_bar);
    let app_boost_bar = (element.kind == Kind::Volume && bar.is_some()).then(|| {
        let boost = make_bar();
        boost.add_css_class("boost-bar");
        boost
    });
    if let (Some(bar), Some(_)) = (&bar, &boost_bar) {
        bar.add_css_class("volume-bar");
    }
    if let Some(app) = &app_bar {
        app.add_css_class("volume-bar");
    }
    // Unless the element says otherwise, a bar is centred and the text lines up by the element's place; otherwise both
    // line up across the way they are stacked as it says.
    let across = match element.child_align {
        ChildAlign::Auto => None,
        ChildAlign::Start => Some(gtk::Align::Start),
        ChildAlign::Center => Some(gtk::Align::Center),
        ChildAlign::End => Some(gtk::Align::End),
        ChildAlign::Fill => Some(gtk::Align::Fill),
    };
    for bar in bar.iter().chain(&boost_bar).chain(&app_bar).chain(&app_boost_bar) {
        if let Some(across) = across {
            if beside {
                bar.set_valign(across);
            } else {
                bar.set_halign(across);
            }
        }
    }
    let (bar_widget, app_volume, video_volume_icon): (Option<gtk::Widget>, Option<gtk::Widget>, Option<gtk::Widget>) =
        match (&bar, &boost_bar) {
            (Some(bar), Some(boost)) => {
                let video = gtk::Overlay::new();
                video.set_child(Some(boost));
                video.add_overlay(bar);
                match (&app_bar, &app_boost_bar) {
                    (Some(app), Some(app_boost)) => {
                        let application = gtk::Overlay::new();
                        application.set_child(Some(app_boost));
                        application.add_overlay(app);
                        let (bars, app_cell, video_icon) = volume_bars(element, video.upcast(), application.upcast());
                        (Some(bars), Some(app_cell), Some(video_icon))
                    }
                    _ => (Some(video.upcast()), None, None),
                }
            }
            (Some(bar), None) => (Some(bar.clone().upcast()), None, None),
            _ => (None, None, None),
        };
    let label = TextLabel::new(element);
    label.widget().add_css_class("overlay-text");
    match &label {
        TextLabel::Plain(plain) => align_label(plain, align),
        TextLabel::Turned(_) => label.widget().set_halign(horizontal_align(align)),
    }
    if beside {
        label.widget().set_valign(across.unwrap_or(gtk::Align::Center));
    } else if let Some(across) = across {
        label.widget().set_halign(across);
    }
    // The text goes before the bar above it or to its left, and after it otherwise.
    let text_first = element.text_position.is_first();
    if let (Some(bar), true) = (&bar_widget, text_first) {
        card.append(&label.widget());
        card.append(bar);
    } else if let Some(bar) = &bar_widget {
        card.append(bar);
        card.append(&label.widget());
    }

    // A media status has an icon beside its text, and the filename under the text, lined up with it.
    let is_status = element.kind == Kind::Status;
    let icon = (is_status && element.shows_part("state")).then(|| {
        let icon = gtk::Label::new(None);
        icon.add_css_class("overlay-text");
        icon.add_css_class("overlay-icon");
        icon.set_valign(across.unwrap_or(gtk::Align::Start));
        // Every state takes the same width, so the text does not move as the state changes.
        icon.set_width_chars(ICON_CHARS);
        icon.set_xalign(0.5);
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
        let grid = gtk::Grid::builder().column_spacing(element.spacing).row_spacing(2).build();
        let column = i32::from(icon.is_some());
        if let Some(icon) = &icon {
            grid.attach(icon, 0, 0, 1, 1);
        }
        label.widget().set_hexpand(true);
        grid.attach(&label.widget(), column, 0, 1, 1);
        if let Some(filename) = &filename {
            filename.set_hexpand(true);
            grid.attach(filename, column, 1, 1, 1);
        }
        card.append(&grid);
    } else if bar.is_none() {
        card.append(&label.widget());
    }

    let revealer = gtk::Revealer::new();
    set_fade(&revealer, element);
    revealer.set_child(Some(&card));
    revealer.set_opacity(element.style.opacity);
    revealer.set_halign(horizontal_align(align));
    revealer.set_can_target(false);
    let sheet = Sheet::new(&format!(
        "{} {} {}",
        style_css(&format!(".overlay-element.{class}"), &element.style),
        effects_css(&format!(".overlay-element.{class}"), &format!(".overlay-element.{class} .overlay-text"), &element.style),
        filename_css(&format!(".overlay-element.{class} .overlay-subtext"), element),
    ));
    Placed {
        index,
        element: element.clone(),
        template: Template::parse(&element.text_template()),
        revealer,
        label,
        shown: RefCell::new(String::new()),
        bar,
        boost_bar,
        app_bar,
        app_boost_bar,
        app_volume,
        video_volume_icon,
        bar_widget,
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
        Frame {
            values,
            fraction: 0.0,
            volume_fraction: 0.0,
            app_volume_fraction: 0.0,
            video_volume_tracked: false,
            has_audio: true,
            is_video: true,
            playing: true,
            fullscreen: false,
            since_change: [None; TRIGGERS],
        }
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
        assert!(element.is_up(&Frame { since_change: [Some(Duration::from_secs(1)), None, None, None, None], ..frame(&none) }));
        assert!(!element.is_up(&Frame { since_change: [Some(Duration::from_secs(3)), None, None, None, None], ..frame(&none) }));

        // A display waits for its own change, not the video being skipped through.
        let mut volume = Element::of_kind(Kind::Volume, Slot::TOP_RIGHT);
        assert!(!volume.is_up(&Frame { since_change: [Some(Duration::from_secs(1)), None, None, None, None], ..frame(&none) }));
        assert!(volume.is_up(&Frame { since_change: [None, Some(Duration::from_secs(1)), None, None, None], ..frame(&none) }));
        volume.visibility = Visibility::Always;
        assert!(volume.is_up(&frame(&none)));

        element.visibility = Visibility::Always;
        element.fullscreen_only = true;
        assert!(!element.is_up(&frame(&none)));
        assert!(element.is_up(&Frame { fullscreen: true, ..frame(&none) }));

        element.enabled = false;
        assert!(!element.is_up(&Frame { fullscreen: true, ..frame(&none) }));
    }
}
