//! The settings a video has from set times on: the viewer applies each keyframe as the video reaches it, sets one from what the
//! video shows now, takes one away, and skips to the one before or after the position. It also zooms and pans the video, which
//! a keyframe can then hold. The keyframes are kept with the file.

use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::{clock, FileSettings, MediaViewer, SEEK_SETTLE};
use crate::media::timeline::{Crop, Look, Timeline};
use crate::media::zoom::PAN_STEP;
use crate::mpv::MpvStream;

/// The file setting that holds the keyframes.
const TIMELINE_KEY: &str = "timeline";

impl MediaViewer {
    /// The stream of the video shown, if mpv plays it.
    fn mpv_stream(&self) -> Option<MpvStream> {
        self.imp().media.borrow().as_ref().and_then(|media| media.downcast_ref::<MpvStream>().cloned())
    }

    /// The stream of the video shown, or none after saying that `what` needs one played by mpv.
    fn mpv_stream_for(&self, what: &str) -> Option<MpvStream> {
        let stream = self.mpv_stream();
        if stream.is_none() {
            self.announce(&format!("{what} needs a video played by mpv"), true);
        }
        stream
    }

    /// Where the video is, in microseconds, or where a skip in progress is taking it.
    fn video_position_micros(&self) -> i64 {
        let imp = self.imp();
        let landing = imp.seek_landing.get().filter(|(_, when)| when.elapsed() < SEEK_SETTLE);
        match (landing, imp.media.borrow().as_ref()) {
            (Some((target, _)), _) => target,
            (None, Some(media)) => media.timestamp(),
            (None, None) => 0,
        }
    }

    /// What the video is shown with now.
    fn current_look(&self, stream: &MpvStream) -> Look {
        let crop = if stream.crop_locked() { stream.crop().map_or(Crop::Whole, Crop::Rect) } else { Crop::Auto };
        Look { orientation: stream.orientation(), zoom: stream.zoom(), crop, speed: stream.speed().unwrap_or(1.0) }
    }

    /// Shows the video as `look` says, changing only what differs, as each change holds up the video.
    fn apply_look(&self, stream: &MpvStream, look: Look) {
        let now = self.current_look(stream);
        if now.orientation != look.orientation {
            stream.set_orientation(look.orientation);
        }
        if now.zoom != look.zoom {
            stream.set_zoom(look.zoom);
        }
        if now.crop != look.crop {
            match look.crop {
                Crop::Auto => stream.unlock_crop(),
                Crop::Whole => stream.lock_crop(None),
                Crop::Rect(rect) => stream.lock_crop(Some(rect)),
            }
        }
        if (now.speed - look.speed).abs() > 1e-6 {
            self.set_speed(look.speed);
        }
    }

    /// Takes the keyframes of the file about to be shown from its settings, none if it has none.
    pub(super) fn load_timeline(&self, settings: Option<&FileSettings>) {
        let timeline = settings.and_then(|settings| settings.get(TIMELINE_KEY)).map(Timeline::from_value).unwrap_or_default();
        self.imp().timeline.replace(timeline);
        self.imp().timeline_applied.set(None);
    }

    /// Applies the keyframe the video has reached, when it is not the one applied. Called as the video's position changes.
    pub(super) fn follow_timeline(&self) {
        let imp = self.imp();
        if imp.timeline.borrow().is_empty() {
            return;
        }
        let Some(stream) = self.mpv_stream() else { return };
        let active = imp.timeline.borrow().active(self.video_position_micros() / 1000);
        match active {
            Some(keyframe) if imp.timeline_applied.get() != Some(keyframe.at_ms) => {
                imp.timeline_applied.set(Some(keyframe.at_ms));
                self.apply_look(&stream, keyframe.look);
            }
            Some(_) => {}
            None => imp.timeline_applied.set(None),
        }
    }

    /// Keeps the keyframes with the file shown.
    fn save_timeline(&self) {
        let imp = self.imp();
        let mut settings = imp.file_settings.borrow_mut().take();
        if settings.is_none() {
            settings = self.shown_path().and_then(|path| FileSettings::for_path(&path).ok());
        }
        let Some(current) = settings.as_mut() else {
            eprintln!("rsimgv: could not keep the timed settings: the file's settings cannot be read");
            return;
        };
        let value = imp.timeline.borrow().to_value();
        if let Err(err) = current.apply([(TIMELINE_KEY, value)]) {
            eprintln!("rsimgv: could not save the timed settings: {err}");
        }
        imp.file_settings.replace(settings);
    }

    /// Makes what the video is shown with now, its turn, mirror, zoom, crop and speed, hold from the position on.
    pub fn timeline_set(&self) {
        let Some(stream) = self.mpv_stream_for("Timed settings") else { return };
        let look = self.current_look(&stream);
        let keyframe = self.imp().timeline.borrow_mut().set(self.video_position_micros() / 1000, look);
        self.imp().timeline_applied.set(Some(keyframe.at_ms));
        self.save_timeline();
        self.announce(&format!("From {}: {look}", clock(keyframe.at_ms * 1000)), false);
    }

    /// Takes away the settings in force at the position, so those before them hold again.
    pub fn timeline_remove(&self) {
        if self.mpv_stream_for("Timed settings").is_none() {
            return;
        }
        let removed = self.imp().timeline.borrow_mut().remove_active(self.video_position_micros() / 1000);
        let Some(keyframe) = removed else {
            self.announce("No timed settings at this point", true);
            return;
        };
        self.imp().timeline_applied.set(None);
        self.save_timeline();
        self.announce(&format!("Removed the settings from {}", clock(keyframe.at_ms * 1000)), false);
        self.follow_timeline();
    }

    /// Takes away every timed setting of the video.
    pub fn timeline_clear(&self) {
        if self.mpv_stream_for("Timed settings").is_none() {
            return;
        }
        let count = self.imp().timeline.borrow().len();
        if count == 0 {
            self.announce("The video has no timed settings", true);
            return;
        }
        self.imp().timeline.borrow_mut().clear();
        self.imp().timeline_applied.set(None);
        self.save_timeline();
        self.announce(&format!("Removed all {count} timed settings"), false);
    }

    /// Skips to where the settings before or after the position start.
    pub fn timeline_seek(&self, forward: bool) {
        if self.mpv_stream_for("Timed settings").is_none() {
            return;
        }
        let position = self.video_position_micros();
        let found = {
            let timeline = self.imp().timeline.borrow();
            if forward { timeline.next(position / 1000) } else { timeline.previous(position / 1000) }
        };
        match found {
            Some(keyframe) => self.seek(keyframe.at_ms * 1000 - position),
            None => self.announce(if forward { "No later timed settings" } else { "No earlier timed settings" }, true),
        }
    }

    /// Zooms the video in or out by `factor`, on the same part of it.
    pub fn zoom_video(&self, factor: f64) {
        let Some(stream) = self.mpv_stream_for("Zooming a video") else { return };
        let zoom = stream.zoom().zoomed(factor);
        stream.set_zoom(zoom);
        self.announce(&format!("Zoom {:.2}×", zoom.level), false);
    }

    /// Shows all of the video again.
    pub fn reset_video_zoom(&self) {
        let Some(stream) = self.mpv_stream_for("Zooming a video") else { return };
        stream.set_zoom(Default::default());
        self.announce("Zoom 1.00×", false);
    }

    /// Moves the zoomed part of the video by `dx` and `dy` steps, to the right and down for positive steps.
    pub fn pan_video(&self, dx: f64, dy: f64) {
        let Some(stream) = self.mpv_stream_for("Panning a video") else { return };
        let zoom = stream.zoom();
        if zoom.is_none() {
            self.announce("Zoom in to pan the video", true);
            return;
        }
        stream.set_zoom(zoom.panned(dx * PAN_STEP, dy * PAN_STEP));
    }
}
