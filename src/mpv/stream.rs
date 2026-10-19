//! A GTK media stream played by libmpv, so a `GtkPicture` and the viewer's controls can use it like a
//! `GtkMediaFile`. libmpv decodes and renders each frame in software into memory, which becomes a texture.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

use super::{Event, Player};

/// The ids observed properties are reported under.
const DURATION: u64 = 1;
const TIME_POS: u64 = 2;
const EOF_REACHED: u64 = 3;

/// The widest a frame is rendered, in pixels; wider videos are scaled down to keep software rendering quick.
const MAX_FRAME_WIDTH: i32 = 2560;

const MICROS: f64 = 1_000_000.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MpvStream {
        pub(super) player: RefCell<Option<Player>>,
        /// The latest frame, and the size frames are rendered at.
        pub(super) frame: RefCell<Option<gdk::Texture>>,
        pub(super) frame_size: Cell<(i32, i32)>,
        pub(super) pixels: RefCell<Vec<u8>>,
        pub(super) duration: Cell<i64>,
        /// Whether a seek is under way, to report when it lands.
        pub(super) seeking: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MpvStream {
        const NAME: &'static str = "RsimgvMpvStream";
        type Type = super::MpvStream;
        type ParentType = gtk::MediaStream;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for MpvStream {
        fn constructed(&self) {
            self.parent_constructed();
            // Looping is done by libmpv, so the end is never reached while it is on.
            self.obj().connect_loop_notify(|stream| {
                if let Some(player) = stream.imp().player.borrow().as_ref() {
                    player.set_string("loop-file", if stream.is_loop() { "inf" } else { "no" });
                }
            });
        }

        fn dispose(&self) {
            self.player.take();
        }
    }

    impl MediaStreamImpl for MpvStream {
        fn play(&self) -> bool {
            let obj = self.obj();
            if let Some(player) = self.player.borrow().as_ref() {
                if obj.is_ended() {
                    let _ = player.command(&["seek", "0", "absolute"]);
                }
                player.set_flag("pause", false);
            }
            true
        }

        fn pause(&self) {
            if let Some(player) = self.player.borrow().as_ref() {
                player.set_flag("pause", true);
            }
        }

        fn seek(&self, timestamp: i64) {
            let seconds = format!("{:.6}", timestamp as f64 / MICROS);
            let result = self.player.borrow().as_ref().map(|player| player.command(&["seek", &seconds, "absolute"]));
            if matches!(result, Some(Ok(()))) {
                self.seeking.set(true);
            } else {
                self.obj().seek_failed();
            }
        }

        fn update_audio(&self, muted: bool, volume: f64) {
            if let Some(player) = self.player.borrow().as_ref() {
                player.set_flag("mute", muted);
                player.set_double("volume", volume * 100.0);
            }
        }
    }

    impl PaintableImpl for MpvStream {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(frame) = self.frame.borrow().as_ref() {
                let snapshot = snapshot.downcast_ref::<gtk::Snapshot>().expect("a GTK snapshot");
                snapshot.append_texture(frame, &gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32));
            }
        }

        fn intrinsic_width(&self) -> i32 {
            self.frame_size.get().0
        }

        fn intrinsic_height(&self) -> i32 {
            self.frame_size.get().1
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            let (width, height) = self.frame_size.get();
            if width > 0 && height > 0 { f64::from(width) / f64::from(height) } else { 0.0 }
        }
    }
}

glib::wrapper! {
    pub struct MpvStream(ObjectSubclass<imp::MpvStream>)
        @extends gtk::MediaStream,
        @implements gdk::Paintable;
}

/// Calls `handle` on the main thread with the stream, once for any number of calls made before it runs.
fn main_thread_waker(stream: &MpvStream, handle: fn(&MpvStream)) -> impl Fn() + Send + Sync + 'static {
    let pending = Arc::new(AtomicBool::new(false));
    let weak = glib::SendWeakRef::from(stream.downgrade());
    move || {
        if pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending = pending.clone();
        let weak = weak.clone();
        glib::MainContext::default().invoke(move || {
            pending.store(false, Ordering::Release);
            if let Some(stream) = weak.upgrade() {
                handle(&stream);
            }
        });
    }
}

impl MpvStream {
    /// A stream of the video in `path`, paused until it is played, with the mpv command line options in
    /// `options`. `None` if libmpv could not start.
    pub fn for_path(path: &Path, options: &str) -> Option<Self> {
        let stream: Self = glib::Object::new();
        let mut player = match Player::new(options) {
            Ok(player) => player,
            Err(err) => {
                eprintln!("rsimgv: mpv: {err}");
                return None;
            }
        };
        player.set_wakeup(main_thread_waker(&stream, MpvStream::handle_events));
        player.set_render_update(main_thread_waker(&stream, MpvStream::render_frame));
        player.observe_double(DURATION, "duration");
        player.observe_double(TIME_POS, "time-pos");
        player.observe_flag(EOF_REACHED, "eof-reached");
        player.set_flag("pause", true);
        if let Err(err) = player.command(&["loadfile", &path.to_string_lossy()]) {
            eprintln!("rsimgv: mpv: could not open {}: {err}", path.display());
            return None;
        }
        stream.imp().player.replace(Some(player));
        // A wakeup may have come before the player was stored, when there was nothing to read the events; libmpv
        // does not wake again while they wait, so they are read now.
        let weak = stream.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(stream) = weak.upgrade() {
                stream.handle_events();
                stream.render_frame();
            }
        });
        Some(stream)
    }

    /// Plays at `speed` times normal speed.
    pub fn set_speed(&self, speed: f64) {
        if let Some(player) = self.imp().player.borrow().as_ref() {
            player.set_double("speed", speed);
        }
    }

    /// The speed it plays at, as a multiple of normal.
    pub fn speed(&self) -> Option<f64> {
        self.imp().player.borrow().as_ref()?.get_double("speed")
    }

    /// Handles the events waiting from libmpv.
    fn handle_events(&self) {
        loop {
            let event = match self.imp().player.borrow().as_ref() {
                Some(player) => player.next_event(),
                None => return,
            };
            let Some(event) = event else { return };
            self.handle_event(event);
        }
    }

    fn handle_event(&self, event: Event) {
        let imp = self.imp();
        match event {
            Event::FileLoaded => {
                let (duration, has_audio) = match imp.player.borrow().as_ref() {
                    Some(player) => (
                        player.get_double("duration").map_or(0, |seconds| (seconds * MICROS) as i64),
                        player.get_i64("current-tracks/audio/id").is_some(),
                    ),
                    None => return,
                };
                imp.duration.set(duration);
                self.update_frame_size();
                if !self.is_prepared() {
                    self.stream_prepared(has_audio, true, true, duration);
                }
                if let Some(player) = imp.player.borrow().as_ref() {
                    player.set_string("loop-file", if self.is_loop() { "inf" } else { "no" });
                    player.set_flag("mute", self.is_muted());
                    player.set_double("volume", self.volume() * 100.0);
                    player.set_flag("pause", !self.is_playing());
                }
            }
            Event::VideoReconfig => self.update_frame_size(),
            Event::PlaybackRestart => {
                if imp.seeking.replace(false) {
                    self.seek_success();
                }
            }
            Event::Double(DURATION, Some(seconds)) => imp.duration.set((seconds * MICROS) as i64),
            Event::Double(TIME_POS, Some(seconds)) => {
                if self.is_prepared() {
                    self.update((seconds * MICROS) as i64);
                }
            }
            Event::Flag(EOF_REACHED, Some(true)) => {
                if self.is_prepared() && !self.is_ended() {
                    self.stream_ended();
                }
            }
            Event::EndFile { error: Some(message) } => {
                self.set_error(glib::Error::new(gtk::glib::FileError::Failed, &message));
            }
            _ => {}
        }
    }

    /// Takes the video's display size, scaled down to the widest frame rendered, and resizes the frames.
    fn update_frame_size(&self) {
        let imp = self.imp();
        let size = imp.player.borrow().as_ref().and_then(|player| Some((player.get_i64("dwidth")?, player.get_i64("dheight")?)));
        let Some((width, height)) = size else { return };
        let (mut width, mut height) = (width as i32, height as i32);
        if width > MAX_FRAME_WIDTH {
            height = (f64::from(height) * f64::from(MAX_FRAME_WIDTH) / f64::from(width)).round() as i32;
            width = MAX_FRAME_WIDTH;
        }
        if width <= 0 || height <= 0 || imp.frame_size.get() == (width, height) {
            return;
        }
        imp.frame_size.set((width, height));
        imp.pixels.replace(vec![0; width as usize * height as usize * 4]);
        self.invalidate_size();
    }

    /// Renders the new frame, if there is one, and shows it.
    fn render_frame(&self) {
        let imp = self.imp();
        let (width, height) = imp.frame_size.get();
        let player = imp.player.borrow();
        let Some(player) = player.as_ref() else { return };
        if !player.frame_ready() || width <= 0 || height <= 0 {
            return;
        }
        let mut pixels = imp.pixels.borrow_mut();
        if let Err(err) = player.render(width, height, &mut pixels) {
            eprintln!("rsimgv: mpv: could not render a frame: {err}");
            return;
        }
        let bytes = glib::Bytes::from(&pixels[..]);
        let texture = gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::B8g8r8x8, &bytes, width as usize * 4);
        drop(pixels);
        imp.frame.replace(Some(texture.upcast()));
        self.invalidate_contents();
    }
}
