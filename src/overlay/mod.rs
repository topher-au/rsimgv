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
    ("speed-changed", "The playback speed, only while it is not normal"),
    ("speed-fraction", "The playback speed as a fraction, such as 3/2"),
    ("speed-percent", "The playback speed in percent, such as 150"),
    ("rotation", "How far the item is turned clockwise, in degrees"),
    ("rotation-percent", "How far it is turned, as 0, 25, 50 or 75 percent of a full turn"),
    ("rotation-name", "How far it is turned, as none, quarter, half or three quarters"),
    ("mirrored", "yes, while the item is mirrored"),
    ("zoom", "The zoom level of a picture, in percent"),
    ("loading", "Loading..., until the video has loaded"),
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
        "speed-changed" => "1.5",
        "speed-fraction" => "3/2",
        "speed-percent" => "150",
        "rotation" => "90",
        "rotation-percent" => "25",
        "rotation-name" => "quarter",
        "zoom" => "125",
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
    /// The volume as a bar, text or both, which comes up when it changes.
    Volume,
    /// The playback speed, which comes up when it changes.
    Speed,
    /// How far the item is turned, which comes up when it changes.
    Rotation,
    /// The zoom level of a picture, which comes up when it changes.
    Zoom,
    /// The state, time and so on of the playback, as mpv's own on-screen display writes them.
    Status,
    /// Where notification messages appear, and how they look.
    Messages,
}

/// What an element that comes up for a while after a change is waiting for the change of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Seek = 0,
    Volume = 1,
    Speed = 2,
    Rotation = 3,
    Zoom = 4,
}

/// How many triggers there are.
pub const TRIGGERS: usize = 5;

impl Kind {
    pub const ALL: [Kind; 8] =
        [Kind::Seek, Kind::Volume, Kind::Speed, Kind::Rotation, Kind::Zoom, Kind::Status, Kind::Messages, Kind::Template];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Template => "template",
            Kind::Seek => "seek",
            Kind::Volume => "volume",
            Kind::Speed => "speed",
            Kind::Rotation => "rotation",
            Kind::Zoom => "zoom",
            Kind::Status => "status",
            Kind::Messages => "messages",
        }
    }

    /// What the kind is called in the page.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Template => "Template",
            Kind::Seek => "Seek display",
            Kind::Volume => "Volume display",
            Kind::Speed => "Speed display",
            Kind::Rotation => "Rotation display",
            Kind::Zoom => "Zoom display",
            Kind::Status => "Media status",
            Kind::Messages => "Notification messages",
        }
    }

    /// The change that brings an element of this kind up, when it is set to come up after one. An element that shows
    /// something else waits for the video to be skipped through.
    pub fn trigger(self) -> Trigger {
        match self {
            Kind::Volume => Trigger::Volume,
            Kind::Speed => Trigger::Speed,
            Kind::Rotation => Trigger::Rotation,
            Kind::Zoom => Trigger::Zoom,
            _ => Trigger::Seek,
        }
    }

    /// Whether the kind is a display of one property that comes up when it changes.
    pub fn is_display(self) -> bool {
        matches!(self, Kind::Seek | Kind::Volume | Kind::Speed | Kind::Rotation | Kind::Zoom)
    }

    fn from_name(name: &str) -> Self {
        match name {
            // The names the first layouts used.
            "progress" => Kind::Seek,
            "seek" => Kind::Seek,
            "volume" => Kind::Volume,
            "speed" => Kind::Speed,
            "rotation" => Kind::Rotation,
            "zoom" => Kind::Zoom,
            "status" => Kind::Status,
            "messages" => Kind::Messages,
            _ => Kind::Template,
        }
    }
}

/// Where the text of a bar display sits beside its bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextPosition {
    Top,
    #[default]
    Bottom,
    Left,
    Right,
}

impl TextPosition {
    pub const ALL: [TextPosition; 4] = [TextPosition::Top, TextPosition::Bottom, TextPosition::Left, TextPosition::Right];

    pub fn name(self) -> &'static str {
        match self {
            TextPosition::Top => "top",
            TextPosition::Bottom => "bottom",
            TextPosition::Left => "left",
            TextPosition::Right => "right",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            TextPosition::Top => "Above the bar",
            TextPosition::Bottom => "Below the bar",
            TextPosition::Left => "Left of the bar",
            TextPosition::Right => "Right of the bar",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|position| position.name() == name).unwrap_or_default()
    }

    /// Whether the text and the bar are side by side rather than one above the other.
    pub fn is_beside(self) -> bool {
        matches!(self, TextPosition::Left | TextPosition::Right)
    }

    /// Whether the text comes before the bar.
    pub fn is_first(self) -> bool {
        matches!(self, TextPosition::Top | TextPosition::Left)
    }
}

/// How a speed display writes the speed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpeedFormat {
    /// `1.5x`
    #[default]
    Multiplier,
    /// `3/2x`
    Fraction,
    /// `150%`
    Percentage,
}

impl SpeedFormat {
    pub const ALL: [SpeedFormat; 3] = [SpeedFormat::Multiplier, SpeedFormat::Fraction, SpeedFormat::Percentage];

    pub fn name(self) -> &'static str {
        match self {
            SpeedFormat::Multiplier => "multiplier",
            SpeedFormat::Fraction => "fraction",
            SpeedFormat::Percentage => "percentage",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            SpeedFormat::Multiplier => "Multiplier, 1.5x",
            SpeedFormat::Fraction => "Fraction, 3/2x",
            SpeedFormat::Percentage => "Percentage, 150%",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|format| format.name() == name).unwrap_or_default()
    }

    fn template(self) -> &'static str {
        match self {
            SpeedFormat::Multiplier => "${speed}x",
            SpeedFormat::Fraction => "${speed-fraction}x",
            SpeedFormat::Percentage => "${speed-percent}%",
        }
    }
}

/// How a rotation display writes how far the item is turned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RotationFormat {
    /// `90°`
    #[default]
    Degrees,
    /// `25%` of a full turn
    Percentage,
    /// `quarter`
    Words,
}

impl RotationFormat {
    pub const ALL: [RotationFormat; 3] = [RotationFormat::Degrees, RotationFormat::Percentage, RotationFormat::Words];

    pub fn name(self) -> &'static str {
        match self {
            RotationFormat::Degrees => "degrees",
            RotationFormat::Percentage => "percentage",
            RotationFormat::Words => "words",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            RotationFormat::Degrees => "Degrees, 0° 90° 180° 270°",
            RotationFormat::Percentage => "Percentage, 0% 25% 50% 75%",
            RotationFormat::Words => "Words, none quarter half three quarters",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|format| format.name() == name).unwrap_or_default()
    }

    fn template(self) -> &'static str {
        match self {
            RotationFormat::Degrees => "${rotation}°${?mirrored: mirrored}",
            RotationFormat::Percentage => "${rotation-percent}%${?mirrored: mirrored}",
            RotationFormat::Words => "${rotation-name}${?mirrored: mirrored}",
        }
    }
}

/// `speed` as the simplest fraction of at most 16ths that is within half a percent of it, such as `3/2`, or a whole number.
pub fn speed_fraction(speed: f64) -> String {
    for denominator in 1..=16u32 {
        let numerator = (speed * f64::from(denominator)).round();
        if numerator >= 1.0 && (numerator / f64::from(denominator) - speed).abs() < 0.005 {
            return if denominator == 1 { format!("{numerator}") } else { format!("{numerator}/{denominator}") };
        }
    }
    format!("{speed:.2}")
}

/// How a rotation of `degrees` clockwise is said in words.
pub fn rotation_name(degrees: u16) -> &'static str {
    match degrees % 360 {
        90 => "quarter",
        180 => "half",
        270 => "three quarters",
        _ => "none",
    }
}

/// The template that writes a volume display's text: the level, or that it is muted.
const VOLUME_TEXT: &str = "${?mute:Muted}${!mute:${volume}%}";

/// The parts a media status element can show, in the order it writes them. The state is an icon and the filename a line
/// under the text, so neither is in the text. The percent and the speed are written together, as `(20% @ 0.2x)`, or
/// whichever of them there is.
pub const STATUS_PARTS: &[(&str, &str)] = &[
    ("state", "State icon"),
    ("time", "Time"),
    ("percent", "Percent"),
    ("speed", "Speed"),
    ("volume", "Volume"),
    ("filename", "Filename under it"),
];

/// How a media status writes the time, and the percent and the speed beside it.
const STATUS_TIME: &str = "${time-pos}${?duration: / ${duration}}";
const STATUS_PERCENT: &str = "${?percent-pos:(${percent-pos}%)}";
const STATUS_SPEED: &str = "${?speed-changed:(${speed-changed}x)}";
const STATUS_PERCENT_AND_SPEED: &str = "${?percent-pos:${?speed-changed:(${percent-pos}% @ ${speed-changed}x)}${!speed-changed:(${percent-pos}%)}}${!percent-pos:${?speed-changed:(${speed-changed}x)}}";
const STATUS_VOLUME: &str = "Vol ${volume}%${?mute: (muted)}";

/// How the filename of a media status fits in the space it has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FilenameWrap {
    /// All on one line, however long.
    SingleLine,
    /// Over several lines, once it is longer than the most characters.
    Wrap,
    /// Cut short with an ellipsis at its start, middle or end, once it is longer than the most characters.
    EllipsizeStart,
    #[default]
    EllipsizeMiddle,
    EllipsizeEnd,
}

impl FilenameWrap {
    pub const ALL: [FilenameWrap; 5] =
        [FilenameWrap::SingleLine, FilenameWrap::Wrap, FilenameWrap::EllipsizeStart, FilenameWrap::EllipsizeMiddle, FilenameWrap::EllipsizeEnd];

    pub fn name(self) -> &'static str {
        match self {
            FilenameWrap::SingleLine => "single-line",
            FilenameWrap::Wrap => "wrap",
            FilenameWrap::EllipsizeStart => "start",
            FilenameWrap::EllipsizeMiddle => "middle",
            FilenameWrap::EllipsizeEnd => "end",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            FilenameWrap::SingleLine => "One line, in full",
            FilenameWrap::Wrap => "Wrap onto more lines",
            FilenameWrap::EllipsizeStart => "Shorten at the start",
            FilenameWrap::EllipsizeMiddle => "Shorten in the middle",
            FilenameWrap::EllipsizeEnd => "Shorten at the end",
        }
    }

    fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|wrap| wrap.name() == name).unwrap_or_default()
    }
}

/// The filename template, and how many characters wide its line goes before it wraps or is shortened.
pub const DEFAULT_FILENAME_TEMPLATE: &str = "${filename}";
pub const FILENAME_CHARS_RANGE: (i32, i32) = (10, 200);
/// How see-through a filename line is unless the element says otherwise.
pub const DEFAULT_FILENAME_OPACITY: f64 = 0.75;

/// How long an element takes to fade in and out unless it says otherwise, and the longest it can be set to.
pub const DEFAULT_FADE_MS: i32 = 200;
pub const FADE_RANGE: (i32, i32) = (0, 3000);

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
    /// The width of a seek display's bar, or the length of a volume display's, in pixels.
    pub bar_width: i32,
    /// For a seek or volume display: whether the bar stands upright, whether it has text as well as the bar, and where the
    /// text sits beside it.
    pub bar_vertical: bool,
    pub show_text: bool,
    pub text_position: TextPosition,
    pub speed_format: SpeedFormat,
    pub rotation_format: RotationFormat,
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
    /// The filename line of a media status: its text, how it fits the space, and its own size (in points, or 0 for a
    /// little under the text's), colour (none for the text's) and opacity.
    pub filename_template: String,
    pub filename_wrap: FilenameWrap,
    pub filename_chars: i32,
    pub filename_font_size: f64,
    pub filename_color: String,
    pub filename_opacity: f64,
    /// Whether the element fades in and out rather than appearing at once, and how long the fade takes, in milliseconds.
    pub fade: bool,
    pub fade_ms: i32,
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
    /// Whether the element has a background, an outline round its letters, and a drop shadow, each in the way set below.
    pub background_on: bool,
    pub stroke_on: bool,
    pub shadow_on: bool,
    /// An outline round the letters: how thick, in pixels (0 for none), and its colour.
    pub stroke_width: f64,
    pub stroke_color: String,
    /// A drop shadow under the text and under the background, and how it falls: how far across and down, how blurred, its
    /// colour and how opaque it is.
    pub text_shadow: bool,
    pub box_shadow: bool,
    pub shadow_x: f64,
    pub shadow_y: f64,
    pub shadow_blur: f64,
    pub shadow_color: String,
    pub shadow_opacity: f64,
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
            background_on: true,
            stroke_on: false,
            shadow_on: true,
            stroke_width: 1.0,
            stroke_color: "rgb(0, 0, 0)".to_owned(),
            text_shadow: true,
            box_shadow: false,
            shadow_x: 0.0,
            shadow_y: 1.0,
            shadow_blur: 3.0,
            shadow_color: "rgb(0, 0, 0)".to_owned(),
            shadow_opacity: 0.9,
        }
    }
}

pub const FONT_SIZE_RANGE: (f64, f64) = (6.0, 96.0);
pub const RADIUS_RANGE: (i32, i32) = (0, 40);
pub const PADDING_RANGE: (i32, i32) = (0, 60);
pub const STROKE_RANGE: (f64, f64) = (0.5, 8.0);
pub const SHADOW_OFFSET_RANGE: (f64, f64) = (-30.0, 30.0);
pub const SHADOW_BLUR_RANGE: (f64, f64) = (0.0, 30.0);
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
            bar_vertical: false,
            show_text: true,
            text_position: TextPosition::default(),
            speed_format: SpeedFormat::default(),
            rotation_format: RotationFormat::default(),
            parts: ["state", "time", "percent"].map(str::to_owned).to_vec(),
            visibility: Visibility::Always,
            seconds: 2.0,
            items: Items::All,
            fullscreen_only: false,
            max_notifications: DEFAULT_MAX_NOTIFICATIONS,
            overflow: Overflow::default(),
            icon_style: IconStyle::default(),
            filename_template: DEFAULT_FILENAME_TEMPLATE.to_owned(),
            filename_wrap: FilenameWrap::default(),
            filename_chars: 40,
            filename_font_size: 0.0,
            filename_color: String::new(),
            filename_opacity: DEFAULT_FILENAME_OPACITY,
            fade: true,
            fade_ms: DEFAULT_FADE_MS,
            style: Style::default(),
        };
        match kind {
            Kind::Seek => {
                element.visibility = Visibility::Seek;
                element.items = Items::Videos;
            }
            Kind::Volume => {
                element.visibility = Visibility::Seek;
                element.items = Items::Videos;
                element.bar_width = 160;
            }
            Kind::Speed => {
                element.visibility = Visibility::Seek;
                element.items = Items::Videos;
            }
            Kind::Rotation => element.visibility = Visibility::Seek,
            Kind::Zoom => {
                element.visibility = Visibility::Seek;
                element.items = Items::Pictures;
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
            Kind::Seek => if self.show_text { time_template(&self.time_format).to_owned() } else { String::new() },
            Kind::Volume => if self.show_text { VOLUME_TEXT.to_owned() } else { String::new() },
            Kind::Speed => self.speed_format.template().to_owned(),
            Kind::Rotation => self.rotation_format.template().to_owned(),
            Kind::Zoom => "${zoom}%".to_owned(),
            Kind::Status => {
                let mut pieces = Vec::new();
                if self.shows_part("time") {
                    pieces.push(STATUS_TIME);
                }
                match (self.shows_part("percent"), self.shows_part("speed")) {
                    (true, true) => pieces.push(STATUS_PERCENT_AND_SPEED),
                    (true, false) => pieces.push(STATUS_PERCENT),
                    (false, true) => pieces.push(STATUS_SPEED),
                    (false, false) => {}
                }
                if self.shows_part("volume") {
                    pieces.push(STATUS_VOLUME);
                }
                // Until the video has loaded there is nothing of it to write.
                format!("${{?loading:Loading...}}${{!loading:{}}}", pieces.join("  "))
            }
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
            "bar-vertical": self.bar_vertical,
            "show-text": self.show_text,
            "text-position": self.text_position.name(),
            "speed-format": self.speed_format.name(),
            "rotation-format": self.rotation_format.name(),
            "parts": self.parts,
            "visibility": match self.visibility { Visibility::Always => "always", Visibility::Seek => "seek", Visibility::Paused => "paused" },
            "seconds": self.seconds,
            "items": match self.items { Items::All => "all", Items::Videos => "videos", Items::Pictures => "pictures" },
            "fullscreen-only": self.fullscreen_only,
            "max-notifications": self.max_notifications,
            "overflow": match self.overflow { Overflow::PushOut => "push-out", Overflow::Wait => "wait" },
            "fade": self.fade,
            "fade-ms": self.fade_ms,
            "filename-template": self.filename_template,
            "filename-wrap": self.filename_wrap.name(),
            "filename-max-chars": self.filename_chars,
            "filename-font-size": self.filename_font_size,
            "filename-color": self.filename_color,
            "filename-opacity": self.filename_opacity,
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
            "background-on": self.style.background_on,
            "stroke-on": self.style.stroke_on,
            "shadow-on": self.style.shadow_on,
            "stroke-width": self.style.stroke_width,
            "stroke-color": self.style.stroke_color,
            "text-shadow": self.style.text_shadow,
            "box-shadow": self.style.box_shadow,
            "shadow-x": self.style.shadow_x,
            "shadow-y": self.style.shadow_y,
            "shadow-blur": self.style.shadow_blur,
            "shadow-color": self.style.shadow_color,
            "shadow-opacity": self.style.shadow_opacity,
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
        element.bar_vertical = flag("bar-vertical", element.bar_vertical);
        element.show_text = flag("show-text", element.show_text);
        if let Some(position) = text("text-position") {
            element.text_position = TextPosition::from_name(position);
        }
        // The first layouts hid a seek display's text with a time format of none.
        if text("time-format") == Some("none") {
            element.show_text = false;
        }
        if let Some(format) = text("speed-format") {
            element.speed_format = SpeedFormat::from_name(format);
        }
        if let Some(format) = text("rotation-format") {
            element.rotation_format = RotationFormat::from_name(format);
        }
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
        element.fade = flag("fade", element.fade);
        element.fade_ms = whole("fade-ms", element.fade_ms, FADE_RANGE);
        if let Some(template) = text("filename-template") {
            element.filename_template = template.to_owned();
        }
        if let Some(wrap) = text("filename-wrap") {
            element.filename_wrap = FilenameWrap::from_name(wrap);
        }
        element.filename_chars = whole("filename-max-chars", element.filename_chars, FILENAME_CHARS_RANGE);
        element.filename_font_size = number("filename-font-size", element.filename_font_size, (0.0, FONT_SIZE_RANGE.1));
        if let Some(color) = text("filename-color") {
            element.filename_color = color.to_owned();
        }
        element.filename_opacity = number("filename-opacity", element.filename_opacity, (0.0, 1.0));
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
        style.background_on = flag("background-on", style.background_on);
        // The first layouts had a stroke when it had a width.
        let had_stroke = object.get("stroke-width").and_then(Value::as_f64).is_some_and(|width| width > 0.0);
        style.stroke_on = flag("stroke-on", had_stroke);
        style.shadow_on = flag("shadow-on", style.shadow_on);
        style.stroke_width = number("stroke-width", style.stroke_width, (0.5, STROKE_RANGE.1)).max(0.5);
        if let Some(color) = text("stroke-color") {
            style.stroke_color = color.to_owned();
        }
        style.text_shadow = flag("text-shadow", style.text_shadow);
        style.box_shadow = flag("box-shadow", style.box_shadow);
        style.shadow_x = number("shadow-x", style.shadow_x, SHADOW_OFFSET_RANGE);
        style.shadow_y = number("shadow-y", style.shadow_y, SHADOW_OFFSET_RANGE);
        style.shadow_blur = number("shadow-blur", style.shadow_blur, SHADOW_BLUR_RANGE);
        if let Some(color) = text("shadow-color") {
            style.shadow_color = color.to_owned();
        }
        style.shadow_opacity = number("shadow-opacity", style.shadow_opacity, (0.0, 1.0));
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

/// The space between the frame's edge and the elements, in pixels, unless a place says otherwise, and the most it can be.
pub const DEFAULT_MARGIN: i32 = 12;
pub const MARGIN_RANGE: (i32, i32) = (0, 200);

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub elements: Vec<Element>,
    /// The margin of each place, row by row from the top left.
    margins: [i32; 9],
}

impl Default for Layout {
    fn default() -> Self {
        Self { elements: Vec::new(), margins: [DEFAULT_MARGIN; 9] }
    }
}

impl Layout {
    /// The layout that looks as the older settings did, with elements that are off, to turn on and change.
    pub fn from_legacy(legacy: &Legacy) -> Self {
        let mut seek = Element::of_kind(Kind::Seek, Slot::from_name(&legacy.seek_position).unwrap_or(Slot::TOP_MIDDLE));
        seek.enabled = legacy.seek_enabled;
        seek.name = "Seek display".to_owned();
        seek.bar = legacy.seek_bar;
        seek.time_format = legacy.seek_time_format.clone();
        seek.show_text = legacy.seek_time;
        seek.visibility = if legacy.seek_always { Visibility::Always } else { Visibility::Seek };
        seek.seconds = legacy.seek_seconds.clamp(SECONDS_RANGE.0, SECONDS_RANGE.1);
        seek.bar_width = legacy.seek_width.clamp(BAR_WIDTH_RANGE.0, BAR_WIDTH_RANGE.1);

        let mut info = Element::new("File info", Slot::from_name(&legacy.status_position).unwrap_or(Slot::BOTTOM_MIDDLE), FILE_INFO_TEMPLATE);
        info.enabled = legacy.status_enabled;
        info.fullscreen_only = true;
        info.style.background_opacity = 0.45;

        let mut elements = vec![seek, info, Element::of_kind(Kind::Messages, Slot::BOTTOM_MIDDLE)];
        elements.extend(Self::examples());
        Self { elements, ..Self::default() }
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

    /// The space between the frame's edge and the elements in `slot`, in pixels. A place in the middle of a side has none on
    /// that side, so this is what its edges take.
    pub fn margin(&self, slot: Slot) -> i32 {
        self.margins[usize::from(slot.row.min(2)) * 3 + usize::from(slot.column.min(2))]
    }

    pub fn set_margin(&mut self, slot: Slot, margin: i32) {
        self.margins[usize::from(slot.row.min(2)) * 3 + usize::from(slot.column.min(2))] = margin.clamp(MARGIN_RANGE.0, MARGIN_RANGE.1);
    }

    pub fn to_json(&self) -> String {
        let margins: Map<String, Value> = Slot::all().map(|slot| (slot.name(), json!(self.margin(slot)))).collect();
        json!({
            "version": VERSION,
            "elements": self.elements.iter().map(Element::to_json).collect::<Vec<_>>(),
            "margins": margins,
        })
        .to_string()
    }

    /// The layout `text` says, or `None` if it is not a layout. An element that is not an object is left out.
    pub fn from_json(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let object: &Map<String, Value> = value.as_object()?;
        let elements = object.get("elements")?.as_array()?;
        let mut layout = Self { elements: elements.iter().filter_map(Element::from_json).collect(), ..Self::default() };
        if let Some(margins) = object.get("margins").and_then(Value::as_object) {
            for slot in Slot::all() {
                if let Some(margin) = margins.get(&slot.name()).and_then(Value::as_f64) {
                    layout.set_margin(slot, margin as i32);
                }
            }
        }
        Some(layout)
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
    fn a_bar_display_places_its_text_and_keeps_its_toggles() {
        let layout = Layout::from_json(r#"{"elements":[{"kind":"seek","text-position":"left","show-text":false,"bar-vertical":true,"background-on":false,"shadow-on":false}]}"#).unwrap();
        let element = &layout.elements[0];
        assert!(element.text_position.is_beside() && element.text_position.is_first());
        assert_eq!(element.text_template(), "", "without text there is none to write");
        assert!(element.bar_vertical && !element.style.background_on && !element.style.shadow_on);
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
        assert!(TextPosition::Right.is_beside() && !TextPosition::Right.is_first());
        assert!(!TextPosition::Bottom.is_beside());
        // The first layouts hid the text with a time format of none.
        let old = Layout::from_json(r#"{"elements":[{"kind":"seek","time-format":"none"}]}"#).unwrap();
        assert!(!old.elements[0].show_text);
    }

    #[test]
    fn speeds_are_written_as_fractions() {
        assert_eq!(speed_fraction(1.0), "1");
        assert_eq!(speed_fraction(0.5), "1/2");
        assert_eq!(speed_fraction(1.5), "3/2");
        assert_eq!(speed_fraction(0.25), "1/4");
        assert_eq!(speed_fraction(2.0), "2");
        assert_eq!(speed_fraction(0.333), "1/3");
        assert_eq!(rotation_name(0), "none");
        assert_eq!(rotation_name(90), "quarter");
        assert_eq!(rotation_name(180), "half");
        assert_eq!(rotation_name(270), "three quarters");
    }

    #[test]
    fn the_displays_come_up_for_the_change_they_show() {
        for (kind, trigger) in [(Kind::Volume, Trigger::Volume), (Kind::Speed, Trigger::Speed), (Kind::Rotation, Trigger::Rotation), (Kind::Zoom, Trigger::Zoom), (Kind::Seek, Trigger::Seek), (Kind::Template, Trigger::Seek)] {
            assert_eq!(kind.trigger(), trigger);
            assert!((trigger as usize) < TRIGGERS);
        }
        let layout = Layout::from_json(r#"{"elements":[{"kind":"volume","speed-format":"fraction"},{"kind":"speed","speed-format":"fraction"},{"kind":"rotation","rotation-format":"words"}]}"#).unwrap();
        assert_eq!(layout.elements[0].visibility, Visibility::Seek, "a display comes up after a change");
        assert_eq!(layout.elements[1].text_template(), "${speed-fraction}x");
        assert_eq!(layout.elements[2].text_template(), "${rotation-name}${?mirrored: mirrored}");
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
        assert!(Kind::ALL.iter().all(|kind| Kind::from_name(kind.name()) == *kind));
    }

    #[test]
    fn the_fade_is_kept_and_held_in_range() {
        let layout = Layout::from_json(r#"{"elements":[{"fade":false,"fade-ms":99999},{}]}"#).unwrap();
        assert_eq!((layout.elements[0].fade, layout.elements[0].fade_ms), (false, FADE_RANGE.1));
        assert_eq!((layout.elements[1].fade, layout.elements[1].fade_ms), (true, DEFAULT_FADE_MS));
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
    }

    #[test]
    fn the_filename_options_are_kept_and_held_in_range() {
        let layout = Layout::from_json(r#"{"elements":[{"kind":"status","filename-wrap":"wrap","filename-max-chars":5000,"filename-template":"${path}"}]}"#).unwrap();
        let element = &layout.elements[0];
        assert_eq!((element.filename_wrap, element.filename_chars, element.filename_template.as_str()), (FilenameWrap::Wrap, FILENAME_CHARS_RANGE.1, "${path}"));
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
        assert_eq!(Element::of_kind(Kind::Status, Slot::TOP_LEFT).filename_wrap, FilenameWrap::EllipsizeMiddle);
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
    fn the_stroke_and_shadows_are_kept_and_held_in_range() {
        let layout = Layout::from_json(r#"{"elements":[{"stroke-width":99,"box-shadow":true,"shadow-x":-99,"text-shadow":false}]}"#).unwrap();
        assert!(layout.elements[0].style.stroke_on, "a stroke with a width is on");
        let style = &layout.elements[0].style;
        assert_eq!((style.stroke_width, style.box_shadow, style.shadow_x, style.text_shadow), (STROKE_RANGE.1, true, SHADOW_OFFSET_RANGE.0, false));
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
        assert!(Style::default().text_shadow && !Style::default().box_shadow && !Style::default().stroke_on && Style::default().background_on);
    }

    #[test]
    fn each_place_keeps_its_own_margin() {
        let mut layout = Layout::default();
        assert!(Slot::all().all(|slot| layout.margin(slot) == DEFAULT_MARGIN));
        layout.set_margin(Slot::TOP_RIGHT, 40);
        layout.set_margin(Slot::BOTTOM_MIDDLE, 9999);
        assert_eq!((layout.margin(Slot::TOP_RIGHT), layout.margin(Slot::TOP_LEFT), layout.margin(Slot::BOTTOM_MIDDLE)), (40, DEFAULT_MARGIN, MARGIN_RANGE.1));
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
        // A layout saved without margins gets the usual one.
        assert_eq!(Layout::from_json(r#"{"elements":[]}"#).map(|layout| layout.margin(Slot::TOP_LEFT)), Some(DEFAULT_MARGIN));
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
            ..Layout::default()
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
            "speed-changed" => Some("0.2".to_owned()),
            _ => None,
        };
        let render = |status: &Element| template::Template::parse(&status.text_template()).render(&lookup);
        assert_eq!(render(&status), "1:23 / 4:56  (28% @ 0.2x)", "the state is an icon, so it is not in the text");
        status.parts = vec!["speed".to_owned(), "time".to_owned(), "filename".to_owned()];
        assert_eq!(render(&status), "1:23 / 4:56  (0.2x)", "in the order it writes them, not the order they were chosen");
        assert!(status.shows_part("filename") && !status.shows_part("state"));

        // The percent and the speed are written as whichever of them there is.
        status.parts = vec!["percent".to_owned(), "speed".to_owned()];
        let only = |status: &Element, present: &[&str]| {
            let present: Vec<String> = present.iter().map(|name| (*name).to_owned()).collect();
            template::Template::parse(&status.text_template()).render(&|name| present.iter().any(|have| have == name).then(|| "1".to_owned()))
        };
        assert_eq!(only(&status, &["percent-pos", "speed-changed"]), "(1% @ 1x)");
        assert_eq!(only(&status, &["percent-pos"]), "(1%)");
        assert_eq!(only(&status, &["speed-changed"]), "(1x)");
        assert_eq!(only(&status, &[]), "");

        // Until the video has loaded, only that is said.
        status.parts = vec!["time".to_owned(), "percent".to_owned()];
        assert_eq!(only(&status, &["loading", "time-pos"]), "Loading...");
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
