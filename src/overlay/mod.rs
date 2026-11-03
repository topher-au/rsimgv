//! The layout of the media overlay: the text, bars and status that float over the viewer's item, and where. The layout
//! is a list of elements, each placed in one of nine places on the item, and is kept as JSON in the `overlay-layout`
//! setting. This module is the model alone; `ui::media_overlay` draws it.

pub mod template;

use serde_json::{json, Map, Value};

/// The properties a template can read, with what each is, for the configuration page to list.
pub const PROPERTIES: &[(&str, &str)] = &[
    ("state", "Playing, Paused or Ended"),
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

/// What an element is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    /// The text its template makes.
    #[default]
    Text,
    /// A bar showing how far into the video, with the template's text under it.
    Progress,
}

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
    pub template: String,
    pub visibility: Visibility,
    /// How long a `Seek` element stays up, in seconds.
    pub seconds: f64,
    pub items: Items,
    pub fullscreen_only: bool,
    /// The size of the text, as a multiple of the usual.
    pub scale: f64,
    /// How see-through the element is, from 0 to 1.
    pub opacity: f64,
    /// Whether the element sits on a translucent card, which keeps its text readable over any picture.
    pub card: bool,
    /// The width of a progress element's bar, in pixels.
    pub bar_width: i32,
}

pub const SCALE_RANGE: (f64, f64) = (0.5, 4.0);
pub const BAR_WIDTH_RANGE: (i32, i32) = (60, 1200);
pub const SECONDS_RANGE: (f64, f64) = (0.5, 30.0);

impl Element {
    pub fn new(name: impl Into<String>, slot: Slot, template: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            kind: Kind::Text,
            slot,
            template: template.into(),
            visibility: Visibility::Always,
            seconds: 2.0,
            items: Items::All,
            fullscreen_only: false,
            scale: 1.0,
            opacity: 1.0,
            card: true,
            bar_width: 360,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "enabled": self.enabled,
            "kind": match self.kind { Kind::Text => "text", Kind::Progress => "progress" },
            "slot": self.slot.name(),
            "template": self.template,
            "visibility": match self.visibility { Visibility::Always => "always", Visibility::Seek => "seek", Visibility::Paused => "paused" },
            "seconds": self.seconds,
            "items": match self.items { Items::All => "all", Items::Videos => "videos", Items::Pictures => "pictures" },
            "fullscreen-only": self.fullscreen_only,
            "scale": self.scale,
            "opacity": self.opacity,
            "card": self.card,
            "bar-width": self.bar_width,
        })
    }

    /// An element from its JSON. What is missing or out of range takes the usual value, so an element written by hand
    /// can say only what it changes. `None` if it is not an object.
    fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let text = |key: &str| object.get(key).and_then(Value::as_str);
        let flag = |key: &str, default: bool| object.get(key).and_then(Value::as_bool).unwrap_or(default);
        let number = |key: &str, default: f64, (low, high): (f64, f64)| {
            object.get(key).and_then(Value::as_f64).map_or(default, |number| number.clamp(low, high))
        };
        let mut element = Element::new(text("name").unwrap_or("Element"), text("slot").and_then(Slot::from_name).unwrap_or(Slot::TOP_LEFT), text("template").unwrap_or_default());
        element.enabled = flag("enabled", true);
        element.kind = if text("kind") == Some("progress") { Kind::Progress } else { Kind::Text };
        element.visibility = match text("visibility") {
            Some("seek") => Visibility::Seek,
            Some("paused") => Visibility::Paused,
            _ => Visibility::Always,
        };
        element.items = match text("items") {
            Some("videos") => Items::Videos,
            Some("pictures") => Items::Pictures,
            _ => Items::All,
        };
        element.seconds = number("seconds", element.seconds, SECONDS_RANGE);
        element.fullscreen_only = flag("fullscreen-only", false);
        element.scale = number("scale", element.scale, SCALE_RANGE);
        element.opacity = number("opacity", element.opacity, (0.1, 1.0));
        element.card = flag("card", true);
        element.bar_width = object
            .get("bar-width")
            .and_then(Value::as_i64)
            .map_or(element.bar_width, |width| width.clamp(i64::from(BAR_WIDTH_RANGE.0), i64::from(BAR_WIDTH_RANGE.1)) as i32);
        Some(element)
    }
}

/// How the older settings wrote the time in the seek overlay, as the templates that say the same.
fn legacy_time_template(format: &str) -> &'static str {
    match format {
        "remaining-total" => "${!duration:${time-pos}}${?duration:-${time-remaining} / ${duration}}",
        "elapsed" => "${time-pos}",
        "remaining" => "${!duration:${time-pos}}${?duration:-${time-remaining}}",
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

/// The status bar's text as a template: the item's number, its name, and what is known of the file.
const STATUS_TEMPLATE: &str =
    "[${playlist-pos}/${playlist-count}]  ${filename}${?resolution:  ${resolution}}${?fps:  ${fps}}${?file-size:  ${file-size}}";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub elements: Vec<Element>,
}

impl Layout {
    /// The layout that looks as the older settings did, with two more elements, off, that show what the overlay can do.
    pub fn from_legacy(legacy: &Legacy) -> Self {
        let mut seek = Element::new("Seek bar", Slot::from_name(&legacy.seek_position).unwrap_or(Slot::TOP_MIDDLE), "");
        seek.enabled = legacy.seek_enabled;
        seek.kind = if legacy.seek_bar { Kind::Progress } else { Kind::Text };
        seek.template = if legacy.seek_time { legacy_time_template(&legacy.seek_time_format).to_owned() } else { String::new() };
        seek.visibility = if legacy.seek_always { Visibility::Always } else { Visibility::Seek };
        seek.seconds = legacy.seek_seconds.clamp(SECONDS_RANGE.0, SECONDS_RANGE.1);
        seek.bar_width = legacy.seek_width.clamp(BAR_WIDTH_RANGE.0, BAR_WIDTH_RANGE.1);
        seek.items = Items::Videos;

        let mut status = Element::new("Status", Slot::from_name(&legacy.status_position).unwrap_or(Slot::BOTTOM_MIDDLE), STATUS_TEMPLATE);
        status.enabled = legacy.status_enabled;
        status.fullscreen_only = true;

        let mut elements = vec![seek, status];
        elements.extend(Self::examples());
        Self { elements }
    }

    /// Elements that are off, to turn on and change: the playback state and time at the top left, and the speed at the
    /// top right.
    pub fn examples() -> Vec<Element> {
        let mut playback = Element::new("Playback", Slot::TOP_LEFT, "${state}  ${time-pos}${?duration: / ${duration}}");
        let mut speed = Element::new("Speed", Slot::TOP_RIGHT, "${speed}x");
        for element in [&mut playback, &mut speed] {
            element.enabled = false;
            element.items = Items::Videos;
        }
        vec![playback, speed]
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
        layout.elements[0].opacity = 0.5;
        layout.elements[2].enabled = true;
        layout.elements[2].visibility = Visibility::Paused;
        assert_eq!(Layout::from_json(&layout.to_json()), Some(layout));
    }

    #[test]
    fn an_element_written_by_hand_says_only_what_it_changes() {
        let layout = Layout::from_json(r#"{"elements":[{"template":"${speed}x","slot":"top-right","scale":99},"nonsense"]}"#).unwrap();
        assert_eq!(layout.elements.len(), 1, "what is not an element is left out");
        let element = &layout.elements[0];
        assert_eq!((element.slot, element.scale, element.enabled, element.card), (Slot::TOP_RIGHT, SCALE_RANGE.1, true, true));
        assert_eq!(Layout::from_json("not json"), None);
        assert_eq!(Layout::from_json("{}"), None);
    }

    #[test]
    fn the_older_settings_become_elements() {
        let layout = Layout::from_legacy(&Legacy { seek_always: true, status_enabled: true, status_position: "top-left".to_owned(), ..legacy() });
        let (seek, status) = (&layout.elements[0], &layout.elements[1]);
        assert_eq!((seek.kind, seek.visibility, seek.slot, seek.items), (Kind::Progress, Visibility::Always, Slot::TOP_MIDDLE, Items::Videos));
        assert!(status.enabled && status.fullscreen_only);
        assert_eq!(status.slot, Slot::TOP_LEFT);
        assert!(layout.elements[2..].iter().all(|element| !element.enabled), "the examples are off");

        let layout = Layout::from_legacy(&Legacy { seek_bar: false, seek_time: false, ..legacy() });
        assert_eq!((layout.elements[0].kind, layout.elements[0].template.as_str()), (Kind::Text, ""));
    }

    #[test]
    fn the_time_templates_read_as_the_formats_did() {
        let lookup = |name: &str| match name {
            "time-pos" => Some("1:23".to_owned()),
            "duration" => Some("4:56".to_owned()),
            "time-remaining" => Some("3:33".to_owned()),
            _ => None,
        };
        let render = |format: &str| template::Template::parse(legacy_time_template(format)).render(&lookup);
        assert_eq!(render("elapsed-total"), "1:23 / 4:56");
        assert_eq!(render("remaining-total"), "-3:33 / 4:56");
        assert_eq!(render("elapsed"), "1:23");
        assert_eq!(render("remaining"), "-3:33");
        // With no duration known, only the time elapsed is.
        let unknown = |name: &str| (name == "time-pos").then(|| "0:05".to_owned());
        for format in ["elapsed-total", "remaining-total", "elapsed", "remaining"] {
            assert_eq!(template::Template::parse(legacy_time_template(format)).render(&unknown), "0:05");
        }
    }
}
