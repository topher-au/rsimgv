//! The control bar floating over a video: previous and next, rewind and fast forward, play and pause,
//! the playback time and duration, a position slider, and a volume button. The button toggles mute,
//! and hovering it reveals a vertical volume slider above it.

use std::cell::{Cell, OnceCell, RefCell};
use std::time::Duration;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

/// How far a rewind or fast forward moves, in microseconds.
pub const SEEK_STEP: i64 = 5_000_000;
/// Microseconds in a second, the unit that media timestamps and durations use.
const MICROS: f64 = 1_000_000.0;
/// How long the pointer may be off the volume button and its slider before the slider closes.
const VOLUME_HIDE_DELAY: Duration = Duration::from_millis(250);

/// The time shown for `micros`: `m:ss`, or `h:mm:ss` from an hour on.
pub fn format_time(micros: i64) -> String {
    let total = micros.max(0) / 1_000_000;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Where a seek by `step` from `current` lands, kept within the media. While the duration is unknown
/// (zero), only the start limits the seek.
pub fn seek_target(current: i64, step: i64, duration: i64) -> i64 {
    let target = (current + step).max(0);
    if duration > 0 { target.min(duration) } else { target }
}

/// The volume button's icon for a volume level and mute state.
pub fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.34 {
        "audio-volume-low-symbolic"
    } else if volume < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/au/topher/rimgv/ui/media_controls.ui")]
    pub struct MediaControls {
        #[template_child]
        pub previous_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rewind_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub forward_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub time_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub position: TemplateChild<gtk::Scale>,
        #[template_child]
        pub volume_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub volume_overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub volume_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub volume_slider_box: TemplateChild<gtk::Box>,
        /// The vertical volume slider, which the revealer shows above the volume button while it is hovered.
        pub(super) volume_scale: OnceCell<gtk::Scale>,
        pub(super) volume_hide: RefCell<Option<glib::SourceId>>,
        /// The video being controlled, if any.
        pub(super) media: RefCell<Option<gtk::MediaFile>>,
        /// The volume and mute state. It outlives each video, so a new one starts at the same level.
        pub(super) volume: Cell<f64>,
        pub(super) muted: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaControls {
        const NAME: &'static str = "RsimgvMediaControls";
        type Type = super::MediaControls;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MediaControls {
        fn constructed(&self) {
            self.parent_constructed();
            self.volume.set(1.0);
            let obj = self.obj();
            let weak = obj.downgrade();

            // The slider lives in the same widget tree as the volume button, rather than in a popover on its
            // own surface, so the pointer moving from the button onto the slider is not read as leaving.
            let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.05);
            scale.set_draw_value(false);
            scale.set_size_request(-1, 120);
            scale.set_value(1.0);
            self.volume_slider_box.append(&scale);
            let _ = self.volume_scale.set(scale.clone());

            let hover = gtk::EventControllerMotion::new();
            hover.connect_enter({
                let weak = weak.clone();
                move |_, _, _| {
                    if let Some(controls) = weak.upgrade() {
                        controls.show_volume();
                    }
                }
            });
            hover.connect_leave({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.hide_volume_later();
                    }
                }
            });
            self.volume_overlay.add_controller(hover);

            self.previous_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.emit_by_name::<()>("previous-requested", &[]);
                    }
                }
            });
            self.next_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.emit_by_name::<()>("next-requested", &[]);
                    }
                }
            });
            self.rewind_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.seek_by(-SEEK_STEP);
                    }
                }
            });
            self.forward_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.seek_by(SEEK_STEP);
                    }
                }
            });
            self.play_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.toggle_playing();
                    }
                }
            });
            self.volume_button.connect_clicked({
                let weak = weak.clone();
                move |_| {
                    if let Some(controls) = weak.upgrade() {
                        controls.toggle_mute();
                    }
                }
            });
            // Only the user's drags and key presses reach these handlers, not the values set in `refresh`.
            self.position.connect_change_value({
                let weak = weak.clone();
                move |_, _, seconds| {
                    if let Some(controls) = weak.upgrade() {
                        controls.seek_to(seconds);
                    }
                    glib::Propagation::Proceed
                }
            });
            scale.connect_change_value({
                let weak = weak.clone();
                move |_, _, volume| {
                    if let Some(controls) = weak.upgrade() {
                        controls.set_volume_level(volume);
                    }
                    glib::Propagation::Proceed
                }
            });

            obj.refresh();
        }

        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("previous-requested").build(),
                    Signal::builder("next-requested").build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for MediaControls {}
    impl BoxImpl for MediaControls {}
}

glib::wrapper! {
    pub struct MediaControls(ObjectSubclass<imp::MediaControls>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MediaControls {
    /// Drives `media` from these controls and shows its state. `None` clears them.
    pub fn set_media(&self, media: Option<&gtk::MediaFile>) {
        self.imp().media.replace(media.cloned());
        if let Some(media) = media {
            // Any change to the media, such as its position or play state, updates the controls.
            let weak = self.downgrade();
            media.connect_notify_local(None, move |_, _| {
                if let Some(controls) = weak.upgrade() {
                    controls.refresh();
                }
            });
            self.apply_audio();
        } else {
            self.refresh();
        }
    }

    /// Enables the previous and next buttons, which are off at either end of the folder.
    pub fn set_navigation(&self, has_previous: bool, has_next: bool) {
        self.imp().previous_button.set_sensitive(has_previous);
        self.imp().next_button.set_sensitive(has_next);
    }

    /// Plays the video if it is paused, and pauses it if it is playing. A video that has ended starts again.
    pub fn toggle_playing(&self) {
        let Some(media) = self.imp().media.borrow().clone() else { return };
        if media.is_playing() {
            media.pause();
        } else {
            if media.is_ended() {
                media.seek(0);
            }
            media.play();
        }
    }

    /// Whether the volume slider is open, which keeps the controls shown while it is used.
    pub fn is_volume_open(&self) -> bool {
        self.imp().volume_revealer.reveals_child()
    }

    /// Calls `callback` when the previous button is clicked.
    pub fn connect_previous_requested<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("previous-requested", false, move |values| {
            let controls = values[0].get::<MediaControls>().expect("controls argument");
            callback(&controls);
            None
        })
    }

    /// Calls `callback` when the next button is clicked.
    pub fn connect_next_requested<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("next-requested", false, move |values| {
            let controls = values[0].get::<MediaControls>().expect("controls argument");
            callback(&controls);
            None
        })
    }

    /// Moves the playback position by `step` microseconds, within the video.
    pub fn seek_by(&self, step: i64) {
        let Some(media) = self.imp().media.borrow().clone() else { return };
        media.seek(seek_target(media.timestamp(), step, media.duration()));
    }

    /// Seeks to `seconds` into the video, as the position slider was dragged to.
    fn seek_to(&self, seconds: f64) {
        if let Some(media) = self.imp().media.borrow().clone() {
            media.seek((seconds * MICROS) as i64);
        }
    }

    /// Changes the volume by `delta`, kept between silence and full. Raising it above silence unmutes.
    pub fn adjust_volume(&self, delta: f64) {
        self.set_volume_level((self.imp().volume.get() + delta).clamp(0.0, 1.0));
    }

    /// Mutes the video, or unmutes it.
    pub fn toggle_mute(&self) {
        let imp = self.imp();
        imp.muted.set(!imp.muted.get());
        self.apply_audio();
    }

    /// Sets the volume from the slider. Raising it above silence unmutes.
    fn set_volume_level(&self, volume: f64) {
        let imp = self.imp();
        imp.volume.set(volume);
        if volume > 0.0 {
            imp.muted.set(false);
        }
        self.apply_audio();
    }

    /// Pushes the volume and mute state to the video.
    fn apply_audio(&self) {
        let imp = self.imp();
        if let Some(media) = imp.media.borrow().as_ref() {
            media.set_volume(imp.volume.get());
            media.set_muted(imp.muted.get());
        }
        self.refresh();
    }

    /// Shows the state of the video: its play button, time, position and volume. A value that is already
    /// shown is not set again, so the sliders are not moved while the user drags them.
    fn refresh(&self) {
        let imp = self.imp();
        let media = imp.media.borrow().clone();
        let (playing, position, duration, seeking) = media
            .as_ref()
            .map_or((false, 0, 0, false), |media| {
                (media.is_playing(), media.timestamp(), media.duration(), media.is_seeking())
            });

        let (icon, tooltip) = if playing { ("media-playback-pause-symbolic", "Pause") } else { ("media-playback-start-symbolic", "Play") };
        imp.play_button.set_icon_name(icon);
        imp.play_button.set_tooltip_text(Some(tooltip));
        imp.time_label.set_label(&format!("{} / {}", format_time(position), format_time(duration)));

        let seconds = duration as f64 / MICROS;
        if imp.position.adjustment().upper() != seconds {
            imp.position.set_range(0.0, seconds);
        }
        if !seeking && (imp.position.value() - position as f64 / MICROS).abs() > 0.01 {
            imp.position.set_value(position as f64 / MICROS);
        }

        let (volume, muted) = (imp.volume.get(), imp.muted.get());
        imp.volume_button.set_icon_name(volume_icon(volume, muted));
        imp.volume_button.set_tooltip_text(Some(if muted { "Unmute" } else { "Mute" }));
        if let Some(scale) = imp.volume_scale.get() {
            if (scale.value() - volume).abs() > 0.001 {
                scale.set_value(volume);
            }
        }
    }

    /// Opens the volume slider, cancelling any pending close.
    fn show_volume(&self) {
        self.cancel_volume_hide();
        self.imp().volume_revealer.set_reveal_child(true);
    }

    /// Closes the volume slider after a short delay, so moving from the button onto the slider keeps it open.
    fn hide_volume_later(&self) {
        let imp = self.imp();
        if imp.volume_hide.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let source = glib::timeout_add_local(VOLUME_HIDE_DELAY, move || {
            let Some(controls) = weak.upgrade() else { return glib::ControlFlow::Break };
            controls.imp().volume_hide.take();
            controls.imp().volume_revealer.set_reveal_child(false);
            glib::ControlFlow::Break
        });
        imp.volume_hide.replace(Some(source));
    }

    fn cancel_volume_hide(&self) {
        if let Some(source) = self.imp().volume_hide.take() {
            source.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_minutes_and_hours() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(65_000_000), "1:05");
        assert_eq!(format_time(3_725_000_000), "1:02:05");
        assert_eq!(format_time(-5), "0:00");
    }

    #[test]
    fn seeks_stay_within_the_media() {
        assert_eq!(seek_target(5_000_000, -SEEK_STEP, 60_000_000), 0);
        assert_eq!(seek_target(55_000_000, SEEK_STEP, 60_000_000), 60_000_000);
        assert_eq!(seek_target(5_000_000, SEEK_STEP, 0), 15_000_000);
    }

    #[test]
    fn volume_icon_follows_level_and_mute() {
        assert_eq!(volume_icon(0.8, true), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(0.0, false), "audio-volume-muted-symbolic");
        assert_eq!(volume_icon(0.2, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon(0.5, false), "audio-volume-medium-symbolic");
        assert_eq!(volume_icon(1.0, false), "audio-volume-high-symbolic");
    }
}
