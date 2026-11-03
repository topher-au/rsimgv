//! A GTK media stream played by libmpv, so a `GtkPicture` and the viewer's controls can use it like a
//! `GtkMediaFile`. libmpv decodes and renders each frame in software into memory, which becomes a texture.
//!
//! One player serves every stream: it is made for the first video, and each later video is loaded into it. The
//! stream made last is the current one; the player's events and frames go to it, and older streams let go of
//! the player. A new player is made only when the mpv options change.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::framing::{self, CropMethod, CropRect, Framing};
use super::{config_generation, Event, Player};
use crate::orientation::Orientation;

/// The ids observed properties are reported under.
const DURATION: u64 = 1;
const TIME_POS: u64 = 2;
const EOF_REACHED: u64 = 3;

/// The widest a frame is rendered, in pixels; wider sizes are scaled down to keep software rendering quick.
const MAX_FRAME_WIDTH: i32 = 2560;

const MICROS: f64 = 1_000_000.0;

/// How often the video is looked at for where to crop it.
const CROP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How far apart, in seconds, the two frames are that the area that moves is found from.
const MOTION_GAP: f64 = 0.1;

/// One greyscale frame of the video at `path`, `seconds` in, decoded at the size `width` by `height`.
fn grab_gray(path: &Path, seconds: f64, width: i32, height: i32) -> Option<Vec<u8>> {
    let frame = crate::fs::thumbnail::run_program(
        std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{seconds:.3}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"]),
    )
    .ok()
    .flatten()?;
    (frame.len() == (width.max(0) * height.max(0)) as usize).then_some(frame)
}

/// Where to crop the video at `path` at `position` seconds, found by `method` from frames decoded from the file. This reads the
/// file, so it runs on a worker thread.
fn measure_frames(method: CropMethod, path: &Path, position: f64, width: i32, height: i32) -> Option<CropRect> {
    let first = grab_gray(path, position, width, height)?;
    let later = (method == CropMethod::Static).then(|| grab_gray(path, position + MOTION_GAP, width, height)).flatten();
    framing::measure(method, &first, later.as_deref(), width as usize, height as usize)
}

/// `width` by `height`, scaled down to the widest frame rendered, keeping the aspect ratio.
fn capped(width: i32, height: i32) -> (i32, i32) {
    if width > MAX_FRAME_WIDTH {
        let height = (f64::from(height) * f64::from(MAX_FRAME_WIDTH) / f64::from(width)).round() as i32;
        return (MAX_FRAME_WIDTH, height);
    }
    (width, height)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MpvStream {
        /// The shared player, while this is the current stream.
        pub(super) player: RefCell<Option<Rc<Player>>>,
        /// The turn and mirror the user gave the video. The player is shared, so each load sets it again.
        pub(super) orientation: Cell<Orientation>,
        /// Whether the player has loaded this stream's file, so events from the file before it are ignored.
        pub(super) loaded: Cell<bool>,
        /// The latest frame, and the video's size, which the frame's intrinsic size is.
        pub(super) frame: RefCell<Option<gdk::Texture>>,
        pub(super) frame_size: Cell<(i32, i32)>,
        /// The size frames are rendered at, in device pixels: the size shown, so mpv's OSD is drawn sharp.
        pub(super) render_size: Cell<(i32, i32)>,
        /// The display's scale factor, which turns the size shown into device pixels.
        pub(super) scale: Cell<i32>,
        pub(super) pixels: RefCell<Vec<u8>>,
        pub(super) duration: Cell<i64>,
        /// Whether a seek is under way, to report when it lands.
        pub(super) seeking: Cell<bool>,
        /// The file this plays, how its frame is to be framed, and the crop in use. The timer looks for where to crop, and the
        /// candidate is the last measurement made from frames, which the next must agree with to be used.
        pub(super) path: RefCell<std::path::PathBuf>,
        pub(super) framing: Cell<Framing>,
        pub(super) crop: Cell<Option<CropRect>>,
        pub(super) crop_candidate: Cell<Option<CropRect>>,
        pub(super) crop_timer: RefCell<Option<glib::SourceId>>,
        pub(super) crop_busy: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MpvStream {
        const NAME: &'static str = "RsimgvMpvStream";
        type Type = super::MpvStream;
        type ParentType = gtk::MediaStream;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for MpvStream {
        fn signals() -> &'static [Signal] {
            use std::sync::LazyLock;
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| vec![Signal::builder("seek-requested").build()]);
            SIGNALS.as_ref()
        }

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
            self.obj().stop();
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
            // Sent before the seek, so a filter taken out in response is out of the way when it lands.
            self.obj().emit_by_name::<()>("seek-requested", &[]);
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
            self.obj().fit_render(width, height);
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

thread_local! {
    /// The shared player, with the mpv options and config generation it was made with.
    static SHARED: RefCell<Option<(String, u64, Rc<Player>)>> = const { RefCell::new(None) };
    /// The stream the shared player is playing for.
    static CURRENT: RefCell<glib::WeakRef<MpvStream>> = RefCell::new(glib::WeakRef::new());
}

/// Calls `handle` on the main thread with the current stream, once for any number of calls made before it runs.
fn main_thread_waker(handle: fn(&MpvStream)) -> impl Fn() + Send + Sync + 'static {
    let pending = Arc::new(AtomicBool::new(false));
    move || {
        if pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending = pending.clone();
        glib::MainContext::default().invoke(move || {
            pending.store(false, Ordering::Release);
            if let Some(stream) = CURRENT.with(|current| current.borrow().upgrade()) {
                handle(&stream);
            }
        });
    }
}

/// The shared player made with `options` and the current mpv.conf, made now if there is none yet or it was made with
/// other options or an older mpv.conf.
fn shared_player(options: &str) -> Option<Rc<Player>> {
    let generation = config_generation();
    if let Some(player) = SHARED.with(|shared| {
        shared
            .borrow()
            .as_ref()
            .filter(|(made_with, made_at, _)| made_with == options && *made_at == generation)
            .map(|(_, _, player)| player.clone())
    }) {
        return Some(player);
    }
    // The old player goes first, so its IPC socket is free for the new one.
    let old = SHARED.with(|shared| shared.take());
    if let Some(stream) = CURRENT.with(|current| current.borrow().upgrade()) {
        stream.imp().player.take();
    }
    drop(old);
    let mut player = match Player::new(options) {
        Ok(player) => player,
        Err(err) => {
            eprintln!("rsimgv: mpv: {err}");
            return None;
        }
    };
    player.set_wakeup(main_thread_waker(MpvStream::handle_events));
    player.set_render_update(main_thread_waker(|stream| stream.render_frame(false)));
    player.observe_double(DURATION, "duration");
    player.observe_double(TIME_POS, "time-pos");
    player.observe_flag(EOF_REACHED, "eof-reached");
    let player = Rc::new(player);
    SHARED.with(|shared| shared.replace(Some((options.to_owned(), generation, player.clone()))));
    Some(player)
}

impl MpvStream {
    /// Stops the video and lets go of the player, which stays alive for the next video. Nothing plays after this.
    pub fn stop(&self) {
        if let Some(timer) = self.imp().crop_timer.take() {
            timer.remove();
        }
        let Some(player) = self.imp().player.take() else { return };
        let _ = player.command(&["stop"]);
        // The player now plays for no stream, unless a later stream has already taken it over.
        CURRENT.with(|current| {
            if current.borrow().upgrade().as_ref() == Some(self) {
                current.replace(glib::WeakRef::new());
            }
        });
    }

    /// A stream of the video in `path`, paused until it is played, loaded into the shared player made with the mpv
    /// command line options in `options`. It becomes the current stream. `None` if libmpv could not start.
    pub fn for_path(path: &Path, options: &str) -> Option<Self> {
        let player = shared_player(options)?;
        let stream: Self = glib::Object::new();
        // The stream before lets go of the player, which now plays for this one.
        let previous = CURRENT.with(|current| current.replace(stream.downgrade()));
        if let Some(previous) = previous.upgrade() {
            previous.imp().player.take();
        }
        player.set_flag("pause", true);
        if let Err(err) = player.command(&["loadfile", &path.to_string_lossy(), "replace"]) {
            eprintln!("rsimgv: mpv: could not open {}: {err}", path.display());
            return None;
        }
        stream.imp().path.replace(path.to_path_buf());
        stream.imp().player.replace(Some(player));
        // Events may already be waiting, with no wakeup to come for them, so they are read now.
        let weak = stream.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(stream) = weak.upgrade() {
                stream.handle_events();
            }
        });
        Some(stream)
    }

    /// Sets how the video is framed: cropped to its picture, and padded over a blurred copy of itself. The crop found for any
    /// video before is let go, and looked for again. The player is shared, so each stream sets this when it starts.
    pub fn set_framing(&self, framing: Framing) {
        let imp = self.imp();
        imp.framing.set(framing);
        imp.crop.set(None);
        imp.crop_candidate.set(None);
        self.apply_framing_filters();
        if let Some(timer) = imp.crop_timer.take() {
            timer.remove();
        }
        if framing.crop.is_none() {
            return;
        }
        let weak = self.downgrade();
        let timer = glib::timeout_add_local(CROP_INTERVAL, move || match weak.upgrade() {
            Some(stream) => {
                stream.look_for_crop();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        imp.crop_timer.replace(Some(timer));
    }

    /// Puts the filters the framing and the crop found call for in the player's chain.
    fn apply_framing_filters(&self) {
        let imp = self.imp();
        let Some(player) = imp.player.borrow().clone() else { return };
        let framing = imp.framing.get();
        let crop = imp.crop.get().map(|rect| framing::trimmed(rect, framing.trim_pixels, player.source_size()));
        let detector = framing.crop == Some(CropMethod::CropDetect);
        player.set_framing_filters(detector, crop, framing.backdrop.map(framing::backdrop_filter));
    }

    /// Looks for where to crop, once an interval: asks the cropdetect filter, or measures a frame decoded from the file.
    fn look_for_crop(&self) {
        let imp = self.imp();
        let Some(player) = imp.player.borrow().clone() else { return };
        let Some(method) = imp.framing.get().crop else { return };
        if !imp.loaded.get() {
            return;
        }
        match method {
            CropMethod::CropDetect => {
                if let Some(rect) = player.detected_crop() {
                    self.use_crop(rect, player.source_size());
                }
            }
            CropMethod::Edges | CropMethod::Static => {
                // A measurement takes a moment, so the next waits for it.
                let (Some(position), Some((width, height))) = (player.get_double("time-pos"), player.source_size()) else { return };
                if imp.crop_busy.replace(true) {
                    return;
                }
                let path = imp.path.borrow().clone();
                let weak = self.downgrade();
                glib::spawn_future_local(async move {
                    let rect = gio::spawn_blocking(move || measure_frames(method, &path, position, width, height)).await.ok().flatten();
                    let Some(stream) = weak.upgrade() else { return };
                    stream.imp().crop_busy.set(false);
                    let Some(rect) = rect else { return };
                    // Two measurements in a row must agree, so a frame that is dark for a moment does not crop the video.
                    let before = stream.imp().crop_candidate.replace(Some(rect));
                    if before.is_some_and(|before| framing::agree(before, rect)) {
                        let size = stream.imp().player.borrow().as_ref().and_then(|player| player.source_size());
                        stream.use_crop(rect, size);
                    }
                });
            }
        }
    }

    /// Crops to `rect`, unless that is the video as it is or little different from the crop in use. A rectangle that covers
    /// the whole video takes the crop off.
    fn use_crop(&self, rect: CropRect, source: Option<(i32, i32)>) {
        let imp = self.imp();
        let whole = source.is_some_and(|(width, height)| rect.left == 0 && rect.top == 0 && rect.width >= width && rect.height >= height);
        let next = (!whole).then_some(rect);
        let differs = match (imp.crop.get(), next) {
            (None, None) => false,
            (Some(old), Some(new)) => framing::changed(old, new),
            _ => true,
        };
        if differs {
            imp.crop.set(next);
            self.apply_framing_filters();
        }
    }

    /// Calls `callback` before each seek is sent to the player.
    pub fn connect_seek_requested<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("seek-requested", false, move |values| {
            let stream = values[0].get::<MpvStream>().expect("stream argument");
            callback(&stream);
            None
        })
    }

    /// Plays at `speed` times normal speed.
    pub fn set_speed(&self, speed: f64) {
        if let Some(player) = self.imp().player.borrow().as_ref() {
            player.set_double("speed", speed);
        }
    }

    /// Turns and mirrors the picture as `orientation` says.
    pub fn set_orientation(&self, orientation: Orientation) {
        self.imp().orientation.set(orientation);
        if let Some(player) = self.imp().player.borrow().as_ref() {
            player.set_orientation(orientation);
        }
    }

    /// The turn and mirror the picture is shown with.
    pub fn orientation(&self) -> Orientation {
        self.imp().orientation.get()
    }

    /// The current frame at the video's own size, captured by mpv, if it is playing for this stream.
    pub fn screenshot(&self) -> Option<gdk::Texture> {
        let player = self.imp().player.borrow().clone()?;
        let frame = match player.screenshot() {
            Ok(frame) => frame,
            Err(err) => {
                eprintln!("rsimgv: mpv: could not capture a frame: {err}");
                return None;
            }
        };
        let bytes = glib::Bytes::from_owned(frame.pixels);
        Some(gdk::MemoryTexture::new(frame.width, frame.height, gdk::MemoryFormat::B8g8r8a8, &bytes, frame.stride).upcast())
    }

    /// The speed it plays at, as a multiple of normal.
    pub fn speed(&self) -> Option<f64> {
        self.imp().player.borrow().as_ref()?.get_double("speed")
    }

    /// The refresh rate of the display the window is on, which mpv reads from the screen.
    pub fn display_fps(&self) -> Option<f64> {
        self.imp().player.borrow().as_ref()?.get_double("display-fps")
    }

    /// Puts `filter`, which is labelled `label`, after the other video filters in place of the one before, or takes that
    /// one out when it is `None`.
    pub fn set_interpolation_filter(&self, label: &str, filter: Option<&str>) -> Result<(), String> {
        let player = self.imp().player.borrow();
        let player = player.as_ref().ok_or("no player")?;
        player.set_interpolation_filter(label, filter)
    }

    /// Turns the video upright, by the rotation its decoder reports, or its container before there is a decoded frame,
    /// and gives it the orientation the user chose.
    fn apply_rotation(&self) {
        if let Some(player) = self.imp().player.borrow().as_ref() {
            let rotation = player.get_i64("video-dec-params/rotate").or_else(|| player.get_i64("current-tracks/video/demux-rotation"));
            player.set_rotation(rotation.unwrap_or(0));
            player.set_orientation(self.imp().orientation.get());
        }
    }

    /// The frame rate the video's container declares, or the one mpv measures when it declares none.
    pub fn fps(&self) -> Option<f64> {
        let player = self.imp().player.borrow();
        let player = player.as_ref()?;
        player.get_double("container-fps").or_else(|| player.get_double("estimated-vf-fps"))
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
                imp.loaded.set(true);
                let (duration, has_audio) = match imp.player.borrow().as_ref() {
                    Some(player) => (
                        player.get_double("duration").map_or(0, |seconds| (seconds * MICROS) as i64),
                        player.get_i64("current-tracks/audio/id").is_some(),
                    ),
                    None => return,
                };
                imp.duration.set(duration);
                self.apply_rotation();
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
            // Until its own file has loaded, what the player reports is about the file before.
            _ if !imp.loaded.get() => {}
            Event::VideoReconfig => {
                self.apply_rotation();
                self.update_frame_size();
            }
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

    /// Takes the video's display size, scaled down to the widest frame rendered, as the intrinsic size.
    fn update_frame_size(&self) {
        let imp = self.imp();
        let size = imp.player.borrow().as_ref().and_then(|player| Some((player.get_i64("dwidth")?, player.get_i64("dheight")?)));
        let Some((width, height)) = size else { return };
        let (width, height) = capped(width as i32, height as i32);
        if width <= 0 || height <= 0 || imp.frame_size.get() == (width, height) {
            return;
        }
        imp.frame_size.set((width, height));
        self.invalidate_size();
    }

    /// Sets the display's scale factor, so frames are rendered at device pixels.
    pub fn set_scale(&self, scale: i32) {
        self.imp().scale.set(scale);
        self.invalidate_contents();
    }

    /// Renders at the size `width` by `height` shown at, in device pixels, when that has changed. Mpv draws its OSD
    /// at the size it renders, so rendering at the video's size and scaling it up would blur the text.
    fn fit_render(&self, width: f64, height: f64) {
        let imp = self.imp();
        let scale = f64::from(imp.scale.get().max(1));
        let (width, height) = capped((width * scale).round() as i32, (height * scale).round() as i32);
        if width <= 0 || height <= 0 || imp.render_size.get() == (width, height) || !self.is_prepared() {
            return;
        }
        imp.render_size.set((width, height));
        imp.pixels.replace(vec![0; width as usize * height as usize * 4]);
        self.render_frame(true);
    }

    /// Renders the new frame, if there is one, or the current frame when `force` is set, and shows it.
    fn render_frame(&self, force: bool) {
        let imp = self.imp();
        let (width, height) = imp.render_size.get();
        let player = imp.player.borrow();
        let Some(player) = player.as_ref() else { return };
        if (!force && !player.frame_ready()) || width <= 0 || height <= 0 {
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
