//! The layout of the media overlay: the text, bars and status that float over the viewer's item, and where. The layout
//! is a list of elements, each placed in one of nine places on the item, and is kept as JSON in the `overlay-layout`
//! setting. This module is the model alone; `ui::media_overlay` draws it.

pub mod template;

use serde_json::{json, Map, Value};

/// The properties a template can read, with what each is, for the configuration page to list.
pub const PROPERTIES: &[(&str, &str)] = &[
    ("state", "Playing, Paused or Ended"),
    ("state-icon", "▶️ while playing, ⏸️ paused, ⏭️ or ⏮️ while skipping, ⏮️ ended"),
    ("time-pos", "The time into the video, such as 1:23"),
    ("duration", "The length of the video"),
    ("time-remaining", "The time left in the video"),
    ("percent-pos", "How far into the video, in percent"),
    ("speed", "The playback speed, such as 1.5"),
    ("volume", "The volume, in percent"),
    ("mute", "muted, while the video is muted"),
    ("fps", "The video's frame rate"),
    ("filename", "The name of the file"),
    ("path", "The full path of the file"),
    ("playlist-pos", "The number of the item among those the viewer steps through"),
    ("playlist-count", "How many items the viewer steps through"),
    ("resolution", "The width and height of the item"),
    ("file-size", "The size of the file"),
];

/// `icon`, an emoji such as ▶️, as the plain character of the font, which the text variation selector asks for.
pub fn plain_icon(icon: &str) -> String {
    icon.chars().filter(|&c| c != '\u{fe0f}').flat_map(|c| [c, '\u{fe0e}']).collect()
}

/// What a property reads as where there is no item to read it from, such as in the editor's preview.
pub fn sample(name: &str) -> Option<String> {
    let value = match name {
        "state" => "Playing",
        "state-icon" => "▶️",
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

/// The version the saved layout is written as, to tell a later layout from this one.
const VERSION: i64 = 1;

/// A place on the item where elements sit, by row and column. Elements in one place stack, the first on top.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Slot {
    pub row: u8,
    pub column: u8,
}

impl Slot {
    pub const TOP_LEFT: Slot = Slot { row: 0, column: 0 };
    pub const TOP_MIDDLE: Slot = Slot { row: 0, column: 1 };
    pub const TOP_RIGHT: Slot = Slot { row: 0, column: 2 };
    pub const BOTTOM_MIDDLE: Slot = Slot { row: 2, column: 1 };

    /// Every place, row by row from the top left.
    pub fn all() -> impl Iterator<Item = Slot> {
        (0..3).flat_map(|row| (0..3).map(move |column| Slot { row, column }))
    }

    /// The name the setting writes the place as, such as `top-left`, `middle-right` or `middle-middle`.
    pub fn name(self) -> String {
        let row = ["top", "middle", "bottom"][usize::from(self.row.min(2))];
        let column = ["left", "middle", "right"][usize::from(self.column.min(2))];
        format!("{row}-{column}")
    }

    /// The place a name stands for. The older settings' positions, such as `bottom-middle`, are places too.
    pub fn from_name(name: &str) -> Option<Self> {
        let (row, column) = name.split_once('-')?;
        let row = ["top", "middle", "bottom"].iter().position(|candidate| *candidate == row)?;
        let column = ["left", "middle", "right"].iter().position(|candidate| *candidate == column)?;
        Some(Slot { row: row as u8, column: column as u8 })
    }
}

/// What an element is, which sets what it shows and what can be changed about it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    /// The text its template makes.
    #[default]
    Template,
    /// A progress bar and the time, which come up when the video is skipped through.
    Seek,
    /// The state, time and so on of the playback, as mpv's own on-screen display writes them.
    Status,
    /// Where notification messages appear, and how they look.
    Messages,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Seek, Kind::Status, Kind::Messages, Kind::Template];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Template => "template",
            Kind::Seek => "seek",
            Kind::Status => "status",
            Kind::Messages => "messages",
        }
    }

    /// What the kind is called in the page.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Template => "Template",
            Kind::Seek => "Seek display",
            Kind::Status => "Media status",
            Kind::Messages => "Notification messages",
        }
    }

    fn from_name(name: &str) -> Self {
        match name {
            // The names the first layouts used.
            "progress" => Kind::Seek,
            "seek" => Kind::Seek,
            "status" => Kind::Status,
            "messages" => Kind::Messages,
            _ => Kind::Template,
        }
    }
}

/// The parts a media status element can show, in the order it writes them, with the template that writes each. The state
/// is an icon and the filename a line under the text, so neither is in the text.
pub const STATUS_PARTS: &[(&str, &str, &str)] = &[
    ("state", "State icon", ""),
    ("time", "Time", "${time-pos}${?duration: / ${duration}}"),
    ("percent", "Percent", "${?percent-pos:(${percent-pos}%)}"),
    ("speed", "Speed", "${?speed:${speed}x}"),
    ("volume", "Volume", "Vol ${volume}%${?mute: (muted)}"),
    ("filename", "Filename under it", ""),
];

/// How a media status draws the playback state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IconStyle {
    /// As emoji, such as ▶️.
    #[default]
    Emoji,
    /// As plain characters of the font, such as ▶.
    Characters,
}

/// What happens to a notification that comes when the stack it would join is full.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// The oldest in the stack is taken down to make room.
    #[default]
    PushOut,
    /// It waits, unseen, until a notification in the stack goes away.
    Wait,
}

/// How many notifications a stack shows at once, unless the element says otherwise, and the most it can be set to.
pub const DEFAULT_MAX_NOTIFICATIONS: i32 = 5;
pub const MAX_NOTIFICATIONS_RANGE: (i32, i32) = (1, 20);

/// How a seek display writes the time.
pub const TIME_FORMATS: &[(&str, &str)] = &[
    ("elapsed-total", "Elapsed / total"),
    ("remaining-total", "Remaining / total"),
    ("elapsed", "Elapsed"),
    ("remaining", "Remaining"),
    ("none", "None"),
];

/// When an element is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Always,
    /// For its `seconds` after the video is skipped through.
    Seek,
    /// While the video is paused.
    Paused,
}

/// What an element is shown over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Items {
    #[default]
    All,
    Videos,
    Pictures,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub name: String,
    pub enabled: bool,
    pub kind: Kind,
    pub slot: Slot,
    /// The text of a template element.
    pub template: String,
    /// What a seek display shows: whether it has a bar, and how it writes the time.
    pub bar: bool,
    pub time_format: String,
    /// The width of a seek display's bar, in pixels.
    pub bar_width: i32,
    /// What a media status shows, from `STATUS_PARTS`.
    pub parts: Vec<String>,
    pub visibility: Visibility,
    /// How long a `Seek` element stays up, in seconds.
    pub seconds: f64,
    pub items: Items,
    pub fullscreen_only: bool,
    /// For notification messages: the most a stack shows at once, and what a notification that comes to a full one does.
    pub max_notifications: i32,
    pub overflow: Overflow,
    /// How a media status draws the playback state.
    pub icon_style: IconStyle,
    pub style: Style,
}

/// How an element looks. The colours are CSS colours without transparency, such as `rgb(0, 0, 0)`; how see-through each
/// is, and the whole element, is set apart.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// The family of the font, or none for the usual one, and its size in points.
    pub font_family: String,
    pub font_size: f64,
    pub text_color: String,
    /// How opaque the text is, from 0 to 1.
    pub text_opacity: f64,
    pub background: String,
    pub background_opacity: f64,
    /// Whether KDE blurs what is behind the element, which needs the KDE effects setting and a Wayland session.
    pub blur: bool,
    /// How round the corners of the background are, and the space between it and the text, in pixels.
    pub radius: i32,
    pub padding: i32,
    /// How opaque the element as a whole is, from 0 to 1.
    pub opacity: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 13.0,
            text_color: "rgb(255, 255, 255)".to_owned(),
            text_opacity: 1.0,
            background: "rgb(0, 0, 0)".to_owned(),
            background_opacity: 0.55,
            blur: false,
            radius: 10,
            padding: 8,
            opacity: 1.0,
        }
    }
}

pub const FONT_SIZE_RANGE: (f64, f64) = (6.0, 96.0);
pub const RADIUS_RANGE: (i32, i32) = (0, 40);
pub const PADDING_RANGE: (i32, i32) = (0, 60);
pub const BAR_WIDTH_RANGE: (i32, i32) = (60, 1200);
pub const SECONDS_RANGE: (f64, f64) = (0.5, 30.0);

impl Slot {
    /// How text in this place lines up: toward the edge of the item the place is nearest, or centred in the middle.
    pub fn text_align(self) -> TextAlign {
        match self.column {
            0 => TextAlign::Left,
            2 => TextAlign::Right,
            _ => TextAlign::Center,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

impl Element {
    /// An element of `kind` with what suits it: a seek display comes up after a skip, a media status stays up.
    pub fn of_kind(kind: Kind, slot: Slot) -> Self {
        let mut element = Self {
            name: kind.title().to_owned(),
            enabled: true,
            kind,
            slot,
            template: String::new(),
            bar: true,
            time_format: TIME_FORMATS[0].0.to_owned(),
            bar_width: 360,
            parts: ["state", "time", "percent"].map(str::to_owned).to_vec(),
            visibility: Visibility::Always,
            seconds: 2.0,
            items: Items::All,
            fullscreen_only: false,
            max_notifications: DEFAULT_MAX_NOTIFICATIONS,
            overflow: Overflow::default(),
            icon_style: IconStyle::default(),
            style: Style::default(),
        };
        match kind {
            Kind::Seek => {
                element.visibility = Visibility::Seek;
                element.items = Items::Videos;
            }
            Kind::Status => element.items = Items::Videos,
            Kind::Template => element.template = "${filename}".to_owned(),
            Kind::Messages => {}
        }
        element
    }

    /// A template element with `template`.
    pub fn new(name: impl Into<String>, slot: Slot, template: impl Into<String>) -> Self {
        Self { name: name.into(), template: template.into(), ..Self::of_kind(Kind::Template, slot) }
    }

    /// The template that makes the element's text: its own for a template element, and for the others the one that
    /// writes what they are set to show.
    pub fn text_template(&self) -> String {
        match self.kind {
            Kind::Template => self.template.clone(),
            Kind::Seek => time_template(&self.time_format).to_owned(),
            Kind::Status => STATUS_PARTS
                .iter()
                .filter(|(name, _, template)| !template.is_empty() && self.parts.iter().any(|part| part == name))
                .map(|(_, _, template)| *template)
                .collect::<Vec<_>>()
                .join("  "),
            Kind::Messages => String::new(),
        }
    }

    /// Whether a media status shows the playback state as an icon, and the filename under its text.
    pub fn shows_part(&self, part: &str) -> bool {
        self.parts.iter().any(|shown| shown == part)
    }

    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "enabled": self.enabled,
            "kind": self.kind.name(),
            "slot": self.slot.name(),
            "template": self.template,
            "bar": self.bar,
            "time-format": self.time_format,
            "bar-width": self.bar_width,
            "parts": self.parts,
            "visibility": match self.visibility { Visibility::Always => "always", Visibility::Seek => "seek", Visibility::Paused => "paused" },
            "seconds": self.seconds,
            "items": match self.items { Items::All => "all", Items::Videos => "videos", Items::Pictures => "pictures" },
            "fullscreen-only": self.fullscreen_only,
            "max-notifications": self.max_notifications,
            "overflow": match self.overflow { Overflow::PushOut => "push-out", Overflow::Wait => "wait" },
            "icon-style": match self.icon_style { IconStyle::Emoji => "emoji", IconStyle::Characters => "characters" },
            "font": self.style.font_family,
            "font-size": self.style.font_size,
            "text-color": self.style.text_color,
            "text-opacity": self.style.text_opacity,
            "background": self.style.background,
            "background-opacity": self.style.background_opacity,
            "blur": self.style.blur,
            "radius": self.style.radius,
            "padding": self.style.padding,
            "opacity": self.style.opacity,
        })
    }

    /// An element from its JSON. What is missing or out of range takes the usual value for its kind, so an element
    /// written by hand can say only what it changes. `None` if it is not an object.
    fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let text = |key: &str| object.get(key).and_then(Value::as_str);
        let flag = |key: &str, default: bool| object.get(key).and_then(Value::as_bool).unwrap_or(default);
        let number = |key: &str, default: f64, (low, high): (f64, f64)| {
            object.get(key).and_then(Value::as_f64).map_or(default, |number| number.clamp(low, high))
        };
        let whole = |key: &str, default: i32, (low, high): (i32, i32)| {
            object.get(key).and_then(Value::as_f64).map_or(default, |number| number.clamp(f64::from(low), f64::from(high)) as i32)
        };
        let kind = Kind::from_name(text("kind").unwrap_or_default());
        let mut element = Element::of_kind(kind, text("slot").and_then(Slot::from_name).unwrap_or(Slot::TOP_LEFT));
        if let Some(name) = text("name") {
            element.name = name.to_owned();
        }
        if let Some(template) = text("template") {
            element.template = template.to_owned();
        }
        element.enabled = flag("enabled", true);
        element.bar = flag("bar", element.bar);
        if let Some(format) = text("time-format").filter(|format| TIME_FORMATS.iter().any(|(name, _)| name == format)) {
            element.time_format = format.to_owned();
        }
        element.bar_width = whole("bar-width", element.bar_width, BAR_WIDTH_RANGE);
        if let Some(parts) = object.get("parts").and_then(Value::as_array) {
            element.parts = parts.iter().filter_map(Value::as_str).map(str::to_owned).collect();
        }
        if let Some(visibility) = text("visibility") {
            element.visibility = match visibility {
                "seek" => Visibility::Seek,
                "paused" => Visibility::Paused,
                _ => Visibility::Always,
            };
        }
        if let Some(items) = text("items") {
            element.items = match items {
                "videos" => Items::Videos,
                "pictures" => Items::Pictures,
                _ => Items::All,
            };
        }
        element.seconds = number("seconds", element.seconds, SECONDS_RANGE);
        element.fullscreen_only = flag("fullscreen-only", false);
        element.max_notifications = whole("max-notifications", element.max_notifications, MAX_NOTIFICATIONS_RANGE);
        if let Some(icons) = text("icon-style") {
            element.icon_style = if icons == "characters" { IconStyle::Characters } else { IconStyle::Emoji };
        }
        if let Some(overflow) = text("overflow") {
            element.overflow = if overflow == "wait" { Overflow::Wait } else { Overflow::PushOut };
        }
        let style = &mut element.style;
        if let Some(family) = text("font") {
            style.font_family = family.to_owned();
        }
        style.font_size = number("font-size", style.font_size, FONT_SIZE_RANGE);
        style.text_opacity = number("text-opacity", style.text_opacity, (0.0, 1.0));
        style.background_opacity = number("background-opacity", style.background_opacity, (0.0, 1.0));
        if let Some(color) = text("text-color") {
            style.text_color = color.to_owned();
        }
        if let Some(color) = text("background") {
            style.background = color.to_owned();
        }
        style.blur = flag("blur", style.blur);
        style.radius = whole("radius", style.radius, RADIUS_RANGE);
        style.padding = whole("padding", style.padding, PADDING_RANGE);
        style.opacity = number("opacity", style.opacity, (0.1, 1.0));
        Some(element)
    }
}

/// How a seek display writes the time, as the template that says it.
fn time_template(format: &str) -> &'static str {
    match format {
        "remaining-total" => "${!duration:${time-pos}}${?duration:-${time-remaining} / ${duration}}",
        "elapsed" => "${time-pos}",
        "remaining" => "${!duration:${time-pos}}${?duration:-${time-remaining}}",
        "none" => "",
        _ => "${time-pos}${?duration: / ${duration}}",
    }
}

/// What the older, separate settings of the seek overlay and the status overlay said.
#[derive(Debug, Clone, PartialEq)]
pub struct Legacy {
    pub seek_enabled: bool,
    pub seek_bar: bool,
    pub seek_time: bool,
    pub seek_time_format: String,
    pub seek_position: String,
    pub seek_seconds: f64,
    pub seek_width: i32,
    pub seek_always: bool,
    pub status_enabled: bool,
    pub status_position: String,
}

/// The text of the status bar under the picture as a template: the item's number, its name, and what is known of the file.
const FILE_INFO_TEMPLATE: &str =
    "[${playlist-pos}/${playlist-count}]  ${filename}${?resolution:  ${resolution}}${?fps:  ${fps}}${?file-size:  ${file-size}}";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub elements: Vec<Element>,
}

impl Layout {
    /// The layout that looks as the older settings did, with elements that are off, to turn on and change.
    pub fn from_legacy(legacy: &Legacy) -> Self {
        let mut seek = Element::of_kind(Kind::Seek, Slot::from_name(&legacy.seek_position).unwrap_or(Slot::TOP_MIDDLE));
        seek.enabled = legacy.seek_enabled;
        seek.name = "Seek bar".to_owned();
        seek.bar = legacy.seek_bar;
        seek.time_format = if legacy.seek_time { legacy.seek_time_format.clone() } else { "none".to_owned() };
        seek.visibility = if legacy.seek_always { Visibility::Always } else { Visibility::Seek };
        seek.seconds = legacy.seek_seconds.clamp(SECONDS_RANGE.0, SECONDS_RANGE.1);
        seek.bar_width = legacy.seek_width.clamp(BAR_WIDTH_RANGE.0, BAR_WIDTH_RANGE.1);

        let mut info = Element::new("File info", Slot::from_name(&legacy.status_position).unwrap_or(Slot::BOTTOM_MIDDLE), FILE_INFO_TEMPLATE);
        info.enabled = legacy.status_enabled;
        info.fullscreen_only = true;
        info.style.background_opacity = 0.45;

        let mut elements = vec![seek, info, Element::of_kind(Kind::Messages, Slot::BOTTOM_MIDDLE)];
        elements.extend(Self::examples());
        Self { elements }
    }

    /// Elements that are off, to turn on and change: the playback status at the top left, and the speed at the top right.
    pub fn examples() -> Vec<Element> {
        let mut status = Element::of_kind(Kind::Status, Slot::TOP_LEFT);
        let mut speed = Element::new("Speed", Slot::TOP_RIGHT, "${speed}x");
        speed.items = Items::Videos;
        for element in [&mut status, &mut speed] {
            element.enabled = false;
        }
        vec![status, speed]
    }

    pub fn to_json(&self) -> String {
        json!({ "version": VERSION, "elements": self.elements.iter().map(Element::to_json).collect::<Vec<_>>() }).to_string()
    }

    /// The layout `text` says, or `None` if it is not a layout. An element that is not an object is left out.
    pub fn from_json(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let object: &Map<String, Value> = value.as_object()?;
        let elements = object.get("elements")?.as_array()?;
        Some(Self { elements: elements.iter().filter_map(Element::from_json).collect() })
    }

    /// Moves the element at `from` into `slot`, in front of the element at `before`, or after the last of those already
    /// in `slot` when there is none. Returns whether anything moved.
    pub fn move_element(&mut self, from: usize, slot: Slot, before: Option<usize>) -> bool {
        if from >= self.elements.len() {
            return false;
        }
        let was = self.elements.clone();
        let mut element = self.elements.remove(from);
        element.slot = slot;
        let at = match before {
            Some(before) if before > from => before - 1,
            Some(before) => before,
            None => self.elements.iter().rposition(|other| other.slot == slot).map_or(self.elements.len(), |last| last + 1),
        };
        self.elements.insert(at.min(self.elements.len()), element);
        self.elements != was
    }

    /// Whether any element is on, which is whether the overlay has anything to keep up to date.
    pub fn any_enabled(&self) -> bool {
        self.elements.iter().any(|element| element.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> Legacy {
        Legacy {
            seek_enabled: true,
            seek_bar: true,
            seek_time: true,
            seek_time_format: "elapsed-total".to_owned(),
            seek_position: "top-middle".to_owned(),
            seek_seconds: 2.0,
            seek_width: 360,
            seek_always: false,
            status_enabled: false,
            status_position: "bottom-middle".to_owned(),
        }
    }

    #[test]
    fn slots_are_named_and_found_by_name() {
        assert_eq!(Slot::all().count(), 9);
        for slot in Slot::all() {
            assert_eq!(Slot::from_name(&slot.name()), Some(slot));
        }
        assert_eq!(Slot::from_name("bottom-middle"), Some(Slot::BOTTOM_MIDDLE));
        assert_eq!(Slot::from_name("middle-middle"), Some(Slot { row: 1, column: 1 }));
        assert_eq!(Slot::from_name("centre"), None);
    }

    #[test]
    fn a_layout_survives_being_saved() {
        let mut layout = Layout::from_legacy(&legacy());
        layout.elements[0].style.opacity = 0.5;
        layout.elements[4].enabled = true;
        layout.elements[4].visibility = Visibility::Paused;
        layout.elements[4].parts = vec!["volume".to_owned()];
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
    }

    #[test]
    fn an_element_written_by_hand_says_only_what_it_changes() {
        let layout = Layout::from_json(
            r#"{"elements":[{"template":"${speed}x","slot":"top-right","font-size":999,"blur":true},{"kind":"seek"},"nonsense"]}"#,
        )
        .unwrap();
        assert_eq!(layout.elements.len(), 2, "what is not an element is left out");
        let (template, seek) = (&layout.elements[0], &layout.elements[1]);
        assert_eq!((template.slot, template.style.font_size, template.enabled, template.style.blur), (Slot::TOP_RIGHT, FONT_SIZE_RANGE.1, true, true));
        assert_eq!((seek.kind, seek.visibility, seek.items), (Kind::Seek, Visibility::Seek, Items::Videos), "a kind brings its usual settings");
        assert_eq!(Layout::from_json("not json"), None);
        assert_eq!(Layout::from_json("{}"), None);
    }

    #[test]
    fn the_names_the_first_layouts_used_still_read() {
        let layout = Layout::from_json(r#"{"elements":[{"kind":"progress"},{"kind":"text"}]}"#).unwrap();
        assert_eq!((layout.elements[0].kind, layout.elements[1].kind), (Kind::Seek, Kind::Template));
    }

    #[test]
    fn the_notification_limits_are_kept_and_held_in_range() {
        let layout = Layout::from_json(r#"{"elements":[{"kind":"messages","max-notifications":99,"overflow":"wait"},{"kind":"messages"}]}"#).unwrap();
        assert_eq!((layout.elements[0].max_notifications, layout.elements[0].overflow), (MAX_NOTIFICATIONS_RANGE.1, Overflow::Wait));
        assert_eq!((layout.elements[1].max_notifications, layout.elements[1].overflow), (DEFAULT_MAX_NOTIFICATIONS, Overflow::PushOut));
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
    }

    #[test]
    fn an_emoji_becomes_the_plain_character() {
        assert_eq!(plain_icon("▶️"), "▶\u{fe0e}");
        assert_eq!(plain_icon("⏭️"), "⏭\u{fe0e}");
        assert_eq!(plain_icon("⏸"), "⏸\u{fe0e}");
        let layout = Layout::from_json(r#"{"elements":[{"kind":"status","icon-style":"characters"}]}"#).unwrap();
        assert_eq!(layout.elements[0].icon_style, IconStyle::Characters);
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
    }

    #[test]
    fn text_lines_up_toward_the_nearest_edge() {
        assert_eq!(Slot::TOP_LEFT.text_align(), TextAlign::Left);
        assert_eq!(Slot { row: 1, column: 1 }.text_align(), TextAlign::Center);
        assert_eq!(Slot::BOTTOM_MIDDLE.text_align(), TextAlign::Center);
        assert_eq!(Slot::TOP_RIGHT.text_align(), TextAlign::Right);
    }

    #[test]
    fn the_older_settings_become_elements() {
        let layout = Layout::from_legacy(&Legacy { seek_always: true, status_enabled: true, status_position: "top-left".to_owned(), ..legacy() });
        let (seek, info, messages) = (&layout.elements[0], &layout.elements[1], &layout.elements[2]);
        assert_eq!((seek.kind, seek.visibility, seek.slot, seek.items), (Kind::Seek, Visibility::Always, Slot::TOP_MIDDLE, Items::Videos));
        assert!(info.enabled && info.fullscreen_only);
        assert_eq!(info.slot, Slot::TOP_LEFT);
        assert_eq!((messages.kind, messages.slot, messages.enabled), (Kind::Messages, Slot::BOTTOM_MIDDLE, true));
        assert!(layout.elements[3..].iter().all(|element| !element.enabled), "the examples are off");

        let layout = Layout::from_legacy(&Legacy { seek_time: false, ..legacy() });
        assert_eq!(layout.elements[0].text_template(), "");
    }

    #[test]
    fn an_element_moves_between_places_and_within_one() {
        let named = |layout: &Layout| layout.elements.iter().map(|element| (element.name.clone(), element.slot)).collect::<Vec<_>>();
        let mut layout = Layout {
            elements: vec![
                Element::new("a", Slot::TOP_LEFT, ""),
                Element::new("b", Slot::TOP_LEFT, ""),
                Element::new("c", Slot::TOP_RIGHT, ""),
            ],
        };
        // To the end of another place.
        assert!(layout.move_element(0, Slot::TOP_RIGHT, None));
        assert_eq!(named(&layout), [("b".into(), Slot::TOP_LEFT), ("c".into(), Slot::TOP_RIGHT), ("a".into(), Slot::TOP_RIGHT)]);
        // In front of another element, which can be before or after it in the list.
        assert!(layout.move_element(2, Slot::TOP_RIGHT, Some(1)));
        assert_eq!(named(&layout), [("b".into(), Slot::TOP_LEFT), ("a".into(), Slot::TOP_RIGHT), ("c".into(), Slot::TOP_RIGHT)]);
        assert!(layout.move_element(0, Slot::TOP_RIGHT, Some(2)));
        assert_eq!(named(&layout), [("a".into(), Slot::TOP_RIGHT), ("b".into(), Slot::TOP_RIGHT), ("c".into(), Slot::TOP_RIGHT)]);
        // To the end of its own place, or in front of itself, is where it already is.
        assert!(!layout.move_element(2, Slot::TOP_RIGHT, None));
        assert!(!layout.move_element(1, Slot::TOP_RIGHT, Some(1)));
        // To an empty place.
        assert!(layout.move_element(1, Slot::BOTTOM_MIDDLE, None));
        assert_eq!(layout.elements.last().map(|element| element.slot), Some(Slot::BOTTOM_MIDDLE));
        assert!(!layout.move_element(9, Slot::TOP_LEFT, None));
    }

    #[test]
    fn a_media_status_writes_the_parts_it_is_set_to_show() {
        let mut status = Element::of_kind(Kind::Status, Slot::TOP_LEFT);
        let lookup = |name: &str| match name {
            "state" => Some("Playing".to_owned()),
            "time-pos" => Some("1:23".to_owned()),
            "duration" => Some("4:56".to_owned()),
            "percent-pos" => Some("28".to_owned()),
            "speed" => Some("1.5".to_owned()),
            _ => None,
        };
        let render = |status: &Element| template::Template::parse(&status.text_template()).render(&lookup);
        assert_eq!(render(&status), "1:23 / 4:56  (28%)", "the state is an icon, so it is not in the text");
        status.parts = vec!["speed".to_owned(), "time".to_owned(), "filename".to_owned()];
        assert_eq!(render(&status), "1:23 / 4:56  1.5x", "in the order it writes them, not the order they were chosen");
        assert!(status.shows_part("filename") && !status.shows_part("state"));
    }

    #[test]
    fn the_time_templates_read_as_the_formats_did() {
        let lookup = |name: &str| match name {
            "time-pos" => Some("1:23".to_owned()),
            "duration" => Some("4:56".to_owned()),
            "time-remaining" => Some("3:33".to_owned()),
            _ => None,
        };
        let render = |format: &str| template::Template::parse(time_template(format)).render(&lookup);
        assert_eq!(render("elapsed-total"), "1:23 / 4:56");
        assert_eq!(render("remaining-total"), "-3:33 / 4:56");
        assert_eq!(render("elapsed"), "1:23");
        assert_eq!(render("remaining"), "-3:33");
        // With no duration known, only the time elapsed is.
        let unknown = |name: &str| (name == "time-pos").then(|| "0:05".to_owned());
        for format in ["elapsed-total", "remaining-total", "elapsed", "remaining"] {
            assert_eq!(template::Template::parse(time_template(format)).render(&unknown), "0:05");
        }
    }
}
