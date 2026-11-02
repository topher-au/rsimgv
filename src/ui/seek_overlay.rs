//! The overlay that shows how far into a video the viewer is: a progress bar and the time, over the video. It comes up
//! when the video is skipped through and goes away after a moment, or stays up while a video plays if the options say so.

use gtk::prelude::*;
use gtk::gio;

use super::media_controls::format_time;

/// How the time is written beside the bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimeFormat {
    /// `1:23 / 4:56`
    #[default]
    ElapsedTotal,
    /// `-3:33 / 4:56`
    RemainingTotal,
    /// `1:23`
    Elapsed,
    /// `-3:33`
    Remaining,
}

impl TimeFormat {
    /// The format a setting value names, or the default for any other.
    pub fn from_name(name: &str) -> Self {
        match name {
            "remaining-total" => Self::RemainingTotal,
            "elapsed" => Self::Elapsed,
            "remaining" => Self::Remaining,
            _ => Self::ElapsedTotal,
        }
    }
}

/// How the overlay is set up, from the settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SeekOptions {
    /// Whether the overlay is shown at all.
    pub enabled: bool,
    /// Whether it has a progress bar, and the time beside it.
    pub bar: bool,
    pub time: bool,
    pub format: TimeFormat,
    /// Where it sits over the video, such as `top-middle`.
    pub position: String,
    /// How long it stays after a skip, in seconds.
    pub seconds: f64,
    /// The width of its progress bar, in pixels.
    pub width: i32,
    /// Whether it stays up while a video plays, rather than only after a skip.
    pub always: bool,
}

impl Default for SeekOptions {
    fn default() -> Self {
        Self { enabled: true, bar: true, time: true, format: TimeFormat::default(), position: "top-middle".to_owned(), seconds: 2.0, width: 360, always: false }
    }
}

impl SeekOptions {
    pub fn from_settings(settings: &gio::Settings) -> Self {
        Self {
            enabled: settings.boolean("seek-overlay"),
            bar: settings.boolean("seek-overlay-bar"),
            time: settings.boolean("seek-overlay-time"),
            format: TimeFormat::from_name(&settings.string("seek-overlay-time-format")),
            position: settings.string("seek-overlay-position").to_string(),
            seconds: settings.double("seek-overlay-seconds"),
            width: settings.int("seek-overlay-width"),
            always: settings.boolean("seek-overlay-always"),
        }
    }
}

/// The time text for a video `position` microseconds in and `duration` long, in `format`. While the duration is unknown
/// (zero), only the time elapsed is known, so that is what is shown.
pub fn time_text(format: TimeFormat, position: i64, duration: i64) -> String {
    let elapsed = format_time(position);
    if duration <= 0 {
        return elapsed;
    }
    let remaining = format!("-{}", format_time(duration - position.clamp(0, duration)));
    let total = format_time(duration);
    match format {
        TimeFormat::ElapsedTotal => format!("{elapsed} / {total}"),
        TimeFormat::RemainingTotal => format!("{remaining} / {total}"),
        TimeFormat::Elapsed => elapsed,
        TimeFormat::Remaining => remaining,
    }
}

/// How far through the video `position` is, from 0 to 1, or 0 while the duration is unknown.
pub fn fraction(position: i64, duration: i64) -> f64 {
    if duration <= 0 {
        return 0.0;
    }
    (position as f64 / duration as f64).clamp(0.0, 1.0)
}

/// The widgets of the overlay, added to the viewer's `gtk::Overlay`.
pub struct SeekOverlay {
    revealer: gtk::Revealer,
    panel: gtk::Box,
    bar: gtk::ProgressBar,
    time: gtk::Label,
}

impl SeekOverlay {
    /// The overlay, over what `overlay` holds, hidden.
    pub fn new(overlay: &gtk::Overlay) -> Self {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
        panel.add_css_class("seek-overlay");
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("seek-bar");
        let time = gtk::Label::new(None);
        time.add_css_class("seek-time");
        panel.append(&bar);
        panel.append(&time);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(150);
        revealer.set_child(Some(&panel));
        // It only shows something, so the video under it takes the clicks.
        revealer.set_can_target(false);
        revealer.set_margin_start(12);
        revealer.set_margin_end(12);
        revealer.set_margin_top(12);
        revealer.set_margin_bottom(12);
        overlay.add_overlay(&revealer);
        Self { revealer, panel, bar, time }
    }

    /// Sets what the overlay shows, how wide it is, and where it sits.
    pub fn apply(&self, options: &SeekOptions) {
        self.bar.set_visible(options.bar);
        self.time.set_visible(options.time);
        self.panel.set_size_request(if options.bar { options.width } else { -1 }, -1);
        self.time.set_halign(if options.bar { gtk::Align::Center } else { gtk::Align::Start });
        let (horizontal, vertical) = super::media_viewer::overlay_alignment(&options.position);
        self.revealer.set_halign(horizontal);
        self.revealer.set_valign(vertical);
    }

    /// Shows the bar and the time for a video `position` microseconds in and `duration` long.
    pub fn update(&self, options: &SeekOptions, position: i64, duration: i64) {
        self.bar.set_fraction(fraction(position, duration));
        self.time.set_label(&time_text(options.format, position, duration));
    }

    pub fn set_shown(&self, shown: bool) {
        self.revealer.set_reveal_child(shown);
    }

    pub fn is_shown(&self) -> bool {
        self.revealer.reveals_child()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000;

    #[test]
    fn the_time_reads_in_the_format_asked_for() {
        let (position, duration) = (83 * SECOND, 296 * SECOND);
        assert_eq!(time_text(TimeFormat::ElapsedTotal, position, duration), "1:23 / 4:56");
        assert_eq!(time_text(TimeFormat::RemainingTotal, position, duration), "-3:33 / 4:56");
        assert_eq!(time_text(TimeFormat::Elapsed, position, duration), "1:23");
        assert_eq!(time_text(TimeFormat::Remaining, position, duration), "-3:33");
        assert_eq!(time_text(TimeFormat::ElapsedTotal, 3_725 * SECOND, 7_200 * SECOND), "1:02:05 / 2:00:00");
    }

    #[test]
    fn an_unknown_duration_shows_only_the_time_elapsed() {
        for format in [TimeFormat::ElapsedTotal, TimeFormat::RemainingTotal, TimeFormat::Elapsed, TimeFormat::Remaining] {
            assert_eq!(time_text(format, 5 * SECOND, 0), "0:05");
        }
        assert_eq!(fraction(5 * SECOND, 0), 0.0);
    }

    #[test]
    fn a_position_past_the_ends_is_held_within_the_video() {
        assert_eq!(time_text(TimeFormat::Remaining, 400 * SECOND, 296 * SECOND), "-0:00");
        assert_eq!(time_text(TimeFormat::Remaining, -SECOND, 60 * SECOND), "-1:00");
        assert_eq!(fraction(400 * SECOND, 296 * SECOND), 1.0);
        assert_eq!(fraction(-SECOND, 296 * SECOND), 0.0);
        assert_eq!(fraction(148 * SECOND, 296 * SECOND), 0.5);
    }

    #[test]
    fn unknown_format_names_fall_back() {
        assert_eq!(TimeFormat::from_name("remaining"), TimeFormat::Remaining);
        assert_eq!(TimeFormat::from_name("nonsense"), TimeFormat::ElapsedTotal);
    }
}
