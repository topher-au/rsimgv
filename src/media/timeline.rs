//! Settings applied to a video from set times on: at 0:00 no rotation, at 0:30 a quarter turn, at 1:15 three. Each
//! keyframe holds everything that can change (turn and mirror, zoom, crop and speed), and is in force until the next one.

use serde_yaml_ng::{Mapping, Value};

use crate::keybindings::actions::{MAX_SPEED, MIN_SPEED};
use crate::media::orientation::Orientation;
use crate::media::zoom::Zoom;
use crate::mpv::framing::CropRect;

/// How near, in milliseconds, a time must be to a keyframe to be taken for it. A video seeked to a keyframe lands a
/// frame or so off it, and a keyframe set just after one fired is that keyframe set again.
const NEAR_MS: i64 = 250;

/// How the video is cropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Crop {
    /// Looked for as the settings say.
    Auto,
    /// Held as the whole frame.
    Whole,
    /// Held as this part of the frame.
    Rect(CropRect),
}

/// Everything a keyframe sets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub orientation: Orientation,
    pub zoom: Zoom,
    pub crop: Crop,
    /// The playback speed, as a multiple of normal.
    pub speed: f64,
}

impl std::fmt::Display for Look {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}°", self.orientation.degrees())?;
        if self.orientation.is_mirrored() {
            write!(f, " mirrored")?;
        }
        write!(f, ", {:.2}× zoom, {:.2}× speed", self.zoom.level, self.speed)?;
        match self.crop {
            Crop::Auto => Ok(()),
            Crop::Whole => write!(f, ", uncropped"),
            Crop::Rect(rect) => write!(f, ", cropped to {} × {}", rect.width, rect.height),
        }
    }
}

/// A look that holds from `at_ms` milliseconds into the video until the next keyframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    pub at_ms: i64,
    pub look: Look,
}

/// The keyframes of a video, in time order, one to a time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    keyframes: Vec<Keyframe>,
}

impl Timeline {
    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keyframes.len()
    }

    /// Sets the look from `at_ms` on, in place of the keyframe at about that time if there is one, which keeps its time.
    /// Returns the keyframe that now holds it.
    pub fn set(&mut self, at_ms: i64, look: Look) -> Keyframe {
        let at_ms = at_ms.max(0);
        if let Some(existing) = self.keyframes.iter_mut().find(|keyframe| (keyframe.at_ms - at_ms).abs() <= NEAR_MS) {
            existing.look = look;
            return *existing;
        }
        let keyframe = Keyframe { at_ms, look };
        let index = self.keyframes.partition_point(|other| other.at_ms < at_ms);
        self.keyframes.insert(index, keyframe);
        keyframe
    }

    /// The keyframe in force at `at_ms`: the last that is not after it. One just after counts, as a video that has been
    /// told to go to a keyframe may stop a little short of it. `None` before the first.
    pub fn active(&self, at_ms: i64) -> Option<Keyframe> {
        self.keyframes.iter().rev().find(|keyframe| keyframe.at_ms <= at_ms + NEAR_MS).copied()
    }

    /// Takes away the keyframe in force at `at_ms`, and returns it.
    pub fn remove_active(&mut self, at_ms: i64) -> Option<Keyframe> {
        let index = self.keyframes.iter().rposition(|keyframe| keyframe.at_ms <= at_ms + NEAR_MS)?;
        Some(self.keyframes.remove(index))
    }

    pub fn clear(&mut self) {
        self.keyframes.clear();
    }

    /// The last keyframe before `at_ms`, leaving out the one the video is at.
    pub fn previous(&self, at_ms: i64) -> Option<Keyframe> {
        self.keyframes.iter().rev().find(|keyframe| keyframe.at_ms < at_ms - NEAR_MS).copied()
    }

    /// The first keyframe after `at_ms`, leaving out the one the video is at.
    pub fn next(&self, at_ms: i64) -> Option<Keyframe> {
        self.keyframes.iter().find(|keyframe| keyframe.at_ms > at_ms + NEAR_MS).copied()
    }

    /// The keyframes as a file setting, or `None` when there are none.
    pub fn to_value(&self) -> Option<Value> {
        (!self.keyframes.is_empty()).then(|| Value::Sequence(self.keyframes.iter().map(keyframe_value).collect()))
    }

    /// The keyframes in a file setting. What is not a keyframe is left out.
    pub fn from_value(value: &Value) -> Self {
        let mut timeline = Self::default();
        for keyframe in value.as_sequence().into_iter().flatten().filter_map(parse_keyframe) {
            timeline.set(keyframe.at_ms, keyframe.look);
        }
        timeline
    }
}

fn keyframe_value(keyframe: &Keyframe) -> Value {
    let look = &keyframe.look;
    let mut map = Mapping::new();
    map.insert("at".into(), keyframe.at_ms.into());
    map.insert("rotation".into(), i64::from(look.orientation.degrees()).into());
    map.insert("mirrored".into(), look.orientation.is_mirrored().into());
    map.insert("zoom".into(), look.zoom.level.into());
    map.insert("pan-x".into(), look.zoom.x.into());
    map.insert("pan-y".into(), look.zoom.y.into());
    map.insert("speed".into(), look.speed.into());
    map.insert(
        "crop".into(),
        match look.crop {
            Crop::Auto => Value::String("auto".to_owned()),
            Crop::Whole => Value::String("whole".to_owned()),
            Crop::Rect(rect) => {
                let mut crop = Mapping::new();
                for (key, value) in [("width", rect.width), ("height", rect.height), ("left", rect.left), ("top", rect.top)] {
                    crop.insert(key.into(), i64::from(value).into());
                }
                Value::Mapping(crop)
            }
        },
    );
    Value::Mapping(map)
}

fn parse_keyframe(value: &Value) -> Option<Keyframe> {
    let at_ms = value.get("at")?.as_i64()?;
    let crop = match value.get("crop") {
        Some(Value::Mapping(crop)) => {
            let side = |key: &str| crop.get(key).and_then(Value::as_i64).and_then(|side| i32::try_from(side).ok());
            Crop::Rect(CropRect { width: side("width")?, height: side("height")?, left: side("left")?, top: side("top")? })
        }
        Some(Value::String(name)) if name == "whole" => Crop::Whole,
        _ => Crop::Auto,
    };
    let look = Look {
        orientation: Orientation::from_parts(value.get("mirrored").and_then(Value::as_bool).unwrap_or(false), value.get("rotation").and_then(Value::as_f64).unwrap_or(0.0)),
        zoom: Zoom {
            level: value.get("zoom").and_then(Value::as_f64).unwrap_or(1.0),
            x: value.get("pan-x").and_then(Value::as_f64).unwrap_or(0.0),
            y: value.get("pan-y").and_then(Value::as_f64).unwrap_or(0.0),
        }
        .clamped(),
        crop,
        speed: value.get("speed").and_then(Value::as_f64).unwrap_or(1.0).clamp(MIN_SPEED, MAX_SPEED),
    };
    Some(Keyframe { at_ms, look })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::zoom::MAX_ZOOM;

    fn look(degrees: f64, speed: f64) -> Look {
        Look { orientation: Orientation::from_parts(false, degrees), zoom: Zoom::default(), crop: Crop::Auto, speed }
    }

    fn timeline() -> Timeline {
        let mut timeline = Timeline::default();
        // Out of order, as they are set while the video is seeked about.
        timeline.set(75_000, look(270.0, 1.0));
        timeline.set(0, look(0.0, 1.0));
        timeline.set(30_000, look(90.0, 2.0));
        timeline
    }

    #[test]
    fn keyframes_are_kept_in_time_order() {
        let times: Vec<i64> = timeline().keyframes.iter().map(|keyframe| keyframe.at_ms).collect();
        assert_eq!(times, [0, 30_000, 75_000]);
    }

    #[test]
    fn setting_near_a_keyframe_replaces_it_and_keeps_its_time() {
        let mut timeline = timeline();
        let set = timeline.set(30_100, look(180.0, 1.5));
        assert_eq!(set.at_ms, 30_000);
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline.active(30_000).map(|keyframe| keyframe.look.orientation.degrees()), Some(180));
    }

    #[test]
    fn the_keyframe_in_force_is_the_last_one_reached() {
        let timeline = timeline();
        assert_eq!(timeline.active(29_000).map(|keyframe| keyframe.at_ms), Some(0));
        assert_eq!(timeline.active(30_000).map(|keyframe| keyframe.at_ms), Some(30_000));
        // A seek that stops a frame short of a keyframe still has it in force.
        assert_eq!(timeline.active(29_900).map(|keyframe| keyframe.at_ms), Some(30_000));
        assert_eq!(timeline.active(120_000).map(|keyframe| keyframe.at_ms), Some(75_000));
        assert_eq!(Timeline::default().active(1_000), None);
        let mut late = Timeline::default();
        late.set(10_000, look(90.0, 1.0));
        assert_eq!(late.active(5_000), None, "nothing is in force before the first keyframe");
    }

    #[test]
    fn removing_takes_away_the_keyframe_in_force() {
        let mut timeline = timeline();
        assert_eq!(timeline.remove_active(40_000).map(|keyframe| keyframe.at_ms), Some(30_000));
        assert_eq!(timeline.active(40_000).map(|keyframe| keyframe.at_ms), Some(0));
        assert_eq!(timeline.len(), 2);
        assert_eq!(Timeline::default().remove_active(0), None);
    }

    #[test]
    fn previous_and_next_skip_the_keyframe_the_video_is_at() {
        let timeline = timeline();
        assert_eq!(timeline.previous(40_000).map(|keyframe| keyframe.at_ms), Some(30_000));
        assert_eq!(timeline.previous(30_050).map(|keyframe| keyframe.at_ms), Some(0));
        assert_eq!(timeline.previous(0), None);
        assert_eq!(timeline.next(40_000).map(|keyframe| keyframe.at_ms), Some(75_000));
        assert_eq!(timeline.next(29_900).map(|keyframe| keyframe.at_ms), Some(75_000));
        assert_eq!(timeline.next(75_000), None);
    }

    #[test]
    fn clearing_leaves_nothing_to_save() {
        let mut timeline = timeline();
        timeline.clear();
        assert!(timeline.is_empty());
        assert_eq!(timeline.to_value(), None);
    }

    #[test]
    fn keyframes_survive_being_saved() {
        let mut timeline = timeline();
        timeline.set(
            90_000,
            Look {
                orientation: Orientation::from_parts(true, 90.0),
                zoom: Zoom { level: 2.5, x: -0.4, y: 0.6 },
                crop: Crop::Rect(CropRect { width: 1280, height: 720, left: 10, top: 20 }),
                speed: 0.5,
            },
        );
        timeline.set(100_000, Look { crop: Crop::Whole, ..look(0.0, 1.0) });
        let value = timeline.to_value().expect("keyframes to save");
        let text = serde_yaml_ng::to_string(&value).unwrap();
        let loaded = Timeline::from_value(&serde_yaml_ng::from_str(&text).unwrap());
        assert_eq!(loaded, timeline);
    }

    #[test]
    fn what_is_not_a_keyframe_is_left_out_and_values_are_kept_in_range() {
        let value: Value = serde_yaml_ng::from_str("[{at: 1000, zoom: 99, speed: 0}, {zoom: 2}, nonsense, {at: 500, crop: {width: 4}}]").unwrap();
        let timeline = Timeline::from_value(&value);
        assert_eq!(timeline.len(), 2);
        let first = timeline.active(1_000).unwrap();
        assert_eq!((first.look.zoom.level, first.look.speed), (MAX_ZOOM, MIN_SPEED));
        assert_eq!(timeline.active(500).unwrap().look.crop, Crop::Auto, "a crop that is not whole is not kept");
    }
}
