//! A GTK media stream played by libmpv, so a `GtkPicture` and the viewer's controls can use it like a
//! `GtkMediaFile`. libmpv decodes each frame and renders it in software into memory on a render thread, and the frame becomes a
//! texture here.
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

use super::framing::{self, Backdrop, CropMethod, CropRect, Framing};
use super::render::FrameImage;
use super::{config_generation, gl, Event, Player, RenderedFrame};
use crate::media::orientation::Orientation;

/// The ids observed properties are reported under.
const DURATION: u64 = 1;
const TIME_POS: u64 = 2;
const EOF_REACHED: u64 = 3;
const SPEED: u64 = 4;
const ESTIMATED_FPS: u64 = 5;

/// The widest a frame is rendered, in pixels; wider sizes are scaled down to keep software rendering quick.
const MAX_FRAME_WIDTH: i32 = 2560;

/// The widest a video can be and still be rendered at its own size, however much bigger it is shown: it is rendered by the CPU,
/// which scales every pixel it is asked for, and GTK scales the frame up on the GPU for little cost. What mpv draws in the frame,
/// such as subtitles and its OSD, is scaled up with it, so it is softer than if it were drawn at the size shown. Wider videos
/// are rendered at the size shown, so that text stays sharp.
const OWN_SIZE_MAX_WIDTH: i32 = 1280;

const MICROS: f64 = 1_000_000.0;

/// How often the video is looked at for where to crop it, at first, and the longest wait between looks once what is found has
/// stopped changing. The wait doubles for each look that finds the crop as it was.
const CROP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const CROP_INTERVAL_MAX: std::time::Duration = std::time::Duration::from_secs(8);

/// How far apart, in seconds, the two frames are that the area that moves is found from.
const MOTION_GAP: f64 = 0.1;

/// How much smaller than the bare strip it fills the backdrop is made, in pixels: it is blurred at that size, which costs
/// little, and the display scales it up as it draws it.
const BACKDROP_SHRINK: f64 = 8.0;

/// How long the window's size must stay as it is before the video is rendered again at the size shown. A size that changes more
/// often than this, such as while the window is dragged, is followed once it settles, with the frame rendered before scaled
/// to fit meanwhile.
const RESIZE_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

/// How often the position is reported to GTK, and so to the controls, while the video plays.
const POSITION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// How long the backdrop is left as it is while the video plays, before it is made again from the frame. It is a blur, so it
/// need not keep up with every frame.
const BACKDROP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// The wait before the next look for the crop, after `settled` looks in a row that found it as it was.
fn crop_delay(settled: u32) -> std::time::Duration {
    (CROP_INTERVAL * 2u32.saturating_pow(settled.min(4))).min(CROP_INTERVAL_MAX)
}

/// The backdrop for an area of the size `area`, from a `frame` with its size: for each bare strip of the area round the picture,
/// the part of the frame that, covering the area, lies under it, shrunk to an eighth, blurred and dimmed, with its size. They are
/// drawn as they are, so only the small pixels are ever blurred. This takes too long for the main thread while a video plays.
fn make_backdrop(frame: &(glib::Bytes, i32, i32), area: (f64, f64), scale: f64, backdrop: Backdrop, scratch: &mut Vec<u8>) -> Vec<(Vec<u8>, usize, usize)> {
    let (pixels, width, height) = frame;
    let (width, height) = (*width, *height);
    let (area_width, area_height) = area;
    let fitted = framing::fitted(area_width, area_height, f64::from(width), f64::from(height));
    let (cover_x, cover_y, cover_width, _) = framing::covering(area_width, area_height, f64::from(width), f64::from(height), backdrop.zoom);
    // Area units per pixel of the frame, when it covers the area.
    let per_pixel = cover_width / f64::from(width);
    let radius = (f64::from(backdrop.blur.max(1)) * 4.0 * scale / BACKDROP_SHRINK).round().max(1.0) as usize;
    let mut pieces = Vec::new();
    for (x, y, strip_width, strip_height) in framing::bars(area_width, area_height, fitted) {
        let out_width = ((strip_width * scale / BACKDROP_SHRINK).ceil() as usize).max(1);
        let out_height = ((strip_height * scale / BACKDROP_SHRINK).ceil() as usize).max(1);
        let region = ((x - cover_x) / per_pixel, (y - cover_y) / per_pixel, (x + strip_width - cover_x) / per_pixel, (y + strip_height - cover_y) / per_pixel);
        let mut small = framing::shrink_region(pixels, width as usize, height as usize, region, out_width, out_height);
        framing::blur_and_dim(&mut small, scratch, out_width, out_height, radius, backdrop.dim);
        pieces.push((small, out_width, out_height));
    }
    pieces
}

/// One greyscale frame of the video at `path`, `seconds` in, scaled by ffmpeg to the size `width` by `height`.
fn grab_gray(path: &Path, seconds: f64, width: i32, height: i32) -> Option<Vec<u8>> {
    let frame = crate::fs::thumbnail::run_program(
        std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &format!("{seconds:.3}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", &format!("scale={width}:{height}:flags=area"), "-f", "rawvideo", "-pix_fmt", "gray", "-"]),
    )
    .ok()
    .flatten()?;
    (frame.len() == (width.max(0) * height.max(0)) as usize).then_some(frame)
}

/// Where to crop the video at `path`, which is `width` by `height`, at `position` seconds, found by `method` from frames decoded
/// from the file. The frames are scaled down by ffmpeg, and the rectangle found in them is scaled back up. This reads the file,
/// so it runs on a worker thread.
fn measure_frames(method: CropMethod, path: &Path, position: f64, width: i32, height: i32) -> Option<CropRect> {
    let (small_width, small_height) = framing::measure_size(width, height);
    let first = grab_gray(path, position, small_width, small_height)?;
    let later = (method == CropMethod::Static).then(|| grab_gray(path, position + MOTION_GAP, small_width, small_height)).flatten();
    let soft_max = framing::soft_pixels(small_width, width);
    let found = framing::measure(method, &first, later.as_deref(), small_width as usize, small_height as usize, soft_max)?;
    Some(framing::scaled_up(found, (small_width, small_height), (width, height)))
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
        /// The volume the player is given, as a fraction of full, when it is above what the stream's own volume allows.
        pub(super) level: Cell<Option<f64>>,
        /// The latest frame, and the video's size, which the frame's intrinsic size is.
        pub(super) frame: RefCell<Option<gdk::Texture>>,
        pub(super) frame_size: Cell<(i32, i32)>,
        /// The size frames are rendered at, in device pixels: the size shown, so mpv's OSD is drawn sharp.
        pub(super) render_size: Cell<(i32, i32)>,
        /// Whether frames are of the whole area, with the backdrop drawn into them by libmpv, instead of the picture alone.
        pub(super) frame_fills: Cell<bool>,
        /// The display's scale, which may be fractional, and turns the size shown into device pixels.
        pub(super) scale: Cell<f64>,
        /// The latest frame with its size, which the backdrop is made from.
        pub(super) last_frame: RefCell<Option<(glib::Bytes, i32, i32)>>,
        /// The size to render at once the window's size settles, when it was last rendered at a new size, and the timer that waits
        /// for it to settle.
        pub(super) wanted_size: Cell<(i32, i32)>,
        pub(super) resized_at: Cell<Option<std::time::Instant>>,
        pub(super) resize_timer: RefCell<Option<glib::SourceId>>,
        /// The size of the area the stream was last drawn into, and the blurred pieces of the backdrop for the bare strips of
        /// it, in order. While the next are made, the ones before are stretched to fit the strips as they are now.
        pub(super) area: Cell<(f64, f64)>,
        pub(super) backdrop: RefCell<Vec<gdk::Texture>>,
        /// When the backdrop was last made, and whether it must be made again from the next frame whatever the time, such as
        /// after a seek. The scratch space is the blur's, kept between backdrops.
        pub(super) backdrop_at: Cell<Option<std::time::Instant>>,
        pub(super) backdrop_stale: Cell<bool>,
        pub(super) blur_scratch: RefCell<Vec<u8>>,
        /// Whether a worker thread is making the backdrop, and whether it must be made again once that is done, as the frame,
        /// the area or the framing changed meanwhile.
        pub(super) backdrop_busy: Cell<bool>,
        pub(super) backdrop_again: Cell<bool>,
        pub(super) duration: Cell<i64>,
        /// The speed and frame rate mpv last reported, so they are read without asking the mpv core, which can be busy. The
        /// frame rate is the container's, or the one measured while playing when it declares none.
        pub(super) speed: Cell<Option<f64>>,
        pub(super) container_fps: Cell<Option<f64>>,
        pub(super) estimated_fps: Cell<Option<f64>>,
        /// Whether a seek is under way, to report when it lands.
        pub(super) seeking: Cell<bool>,
        /// The latest position mpv gave, in microseconds; when GTK was last given one; and the timer that gives it the latest
        /// once the interval is up, if some were held back.
        pub(super) position: Cell<i64>,
        pub(super) position_at: Cell<Option<std::time::Instant>>,
        pub(super) position_timer: RefCell<Option<glib::SourceId>>,
        /// The file this plays, how its frame is to be framed, and the crop in use. The timer looks for where to crop, and the
        /// candidate is the last measurement made from frames, which the next must agree with to be used.
        pub(super) path: RefCell<std::path::PathBuf>,
        pub(super) framing: Cell<Framing>,
        pub(super) crop: Cell<Option<CropRect>>,
        pub(super) crop_candidate: Cell<Option<CropRect>>,
        pub(super) crop_timer: RefCell<Option<glib::SourceId>>,
        pub(super) crop_busy: Cell<bool>,
        /// How many looks in a row found the crop as it was, which lengthens the wait for the next, and the position the last
        /// look was at, so a paused video is not looked at again until it moves.
        pub(super) crop_settled: Cell<u32>,
        pub(super) crop_looked_at: Cell<Option<i64>>,
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
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| vec![Signal::builder("seek-requested").build(), Signal::builder("first-frame").build()]);
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
            self.obj().report_position(true);
        }

        fn seek(&self, timestamp: i64) {
            // Sent before the seek, so a filter taken out in response is out of the way when it lands.
            self.obj().emit_by_name::<()>("seek-requested", &[]);
            self.backdrop_stale.set(true);
            // Where it lands may have other bars.
            self.crop_settled.set(0);
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
                player.set_double("volume", self.level.get().unwrap_or(volume) * 100.0);
            }
        }
    }

    impl PaintableImpl for MpvStream {
        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let stream = self.obj();
            stream.fit_render(width, height);
            let Some(frame) = self.frame.borrow().clone() else { return };
            let snapshot = snapshot.downcast_ref::<gtk::Snapshot>().expect("a GTK snapshot");
            // The frame is the picture alone, as big as fits. The blurred, dimmed pieces of it, if there is a backdrop, fill the
            // bare strips round it, and otherwise they are black.
            if self.framing.get().backdrop.is_none() {
                snapshot.append_color(&gdk::RGBA::BLACK, &gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32));
            }
            let (frame_width, frame_height) = (f64::from(frame.width()), f64::from(frame.height()));
            if self.frame_fills.get() {
                // The frame has the backdrop drawn into it. While one at the new size is on its way after a resize, stretched it
                // only fills the edges, which are a blur, and the picture in it is laid over that at its right shape and place.
                let whole = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
                snapshot.append_texture(&frame, &whole);
                let (video_width, video_height) = self.frame_size.get();
                if let Some((x, y, shown_width, shown_height)) = framing::resized_frame((width, height), (frame_width, frame_height), (f64::from(video_width), f64::from(video_height))) {
                    snapshot.push_clip(&whole);
                    snapshot.append_texture(&frame, &gtk::graphene::Rect::new(x as f32, y as f32, shown_width as f32, shown_height as f32));
                    snapshot.pop();
                }
                return;
            }
            let (x, y, shown_width, shown_height) = framing::fitted(width, height, frame_width, frame_height);
            let strips = framing::bars(width, height, (x, y, shown_width, shown_height));
            for (piece, (strip_x, strip_y, strip_width, strip_height)) in self.backdrop.borrow().iter().zip(strips) {
                let strip = gtk::graphene::Rect::new(strip_x as f32, strip_y as f32, strip_width as f32, strip_height as f32);
                snapshot.append_scaled_texture(piece, gtk::gsk::ScalingFilter::Linear, &strip);
            }
            snapshot.append_texture(&frame, &gtk::graphene::Rect::new(x as f32, y as f32, shown_width as f32, shown_height as f32));
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
    /// The shared player, with the mpv options, whether it renders with OpenGL and the config generation it was made with.
    static SHARED: RefCell<Option<(String, bool, u64, Rc<Player>)>> = const { RefCell::new(None) };
    /// The stream the shared player is playing for.
    static CURRENT: RefCell<glib::WeakRef<MpvStream>> = RefCell::new(glib::WeakRef::new());
    /// Whether the shared player renders with OpenGL, as the preferences ask.
    static RENDER_WITH_OPENGL: Cell<bool> = const { Cell::new(false) };
}

/// Sets whether videos are rendered with OpenGL. The next video opened uses it.
pub fn set_render_with_opengl(on: bool) {
    RENDER_WITH_OPENGL.with(|flag| flag.set(on));
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

/// A player that renders with OpenGL, if the preferences ask for one and it can be made.
fn gl_player(options: &str) -> Option<Player> {
    if !RENDER_WITH_OPENGL.with(Cell::get) {
        return None;
    }
    let made = gl::new_context().and_then(|context| Player::new(options, Some(context)));
    if let Err(err) = &made {
        eprintln!("rsimgv: mpv: could not render with OpenGL, so rendering in software: {err}");
    }
    made.ok()
}

/// The shared player made with `options` and the current mpv.conf, made now if there is none yet or it was made with
/// other options or an older mpv.conf.
fn shared_player(options: &str) -> Option<Rc<Player>> {
    let generation = config_generation();
    let opengl = RENDER_WITH_OPENGL.with(Cell::get);
    if let Some(player) = SHARED.with(|shared| {
        shared
            .borrow()
            .as_ref()
            .filter(|(made_with, made_opengl, made_at, _)| made_with == options && *made_opengl == opengl && *made_at == generation)
            .map(|(_, _, _, player)| player.clone())
    }) {
        return Some(player);
    }
    // The old player goes first, so its IPC socket is free for the new one.
    let old = SHARED.with(|shared| shared.take());
    if let Some(stream) = CURRENT.with(|current| current.borrow().upgrade()) {
        stream.imp().player.take();
    }
    drop(old);
    let mut player = match gl_player(options).map_or_else(|| Player::new(options, None), Ok) {
        Ok(player) => player,
        Err(err) => {
            eprintln!("rsimgv: mpv: {err}");
            return None;
        }
    };
    player.set_wakeup(main_thread_waker(MpvStream::handle_events));
    // Frames are rendered on a thread of their own, and made into textures here.
    let id = player.id();
    player.start_renderer(move |frame| {
        glib::MainContext::default().invoke(move || {
            if let Some(stream) = CURRENT.with(|current| current.borrow().upgrade()) {
                stream.frame_rendered(id, frame);
            }
        });
    });
    player.observe_double(DURATION, "duration");
    player.observe_double(TIME_POS, "time-pos");
    player.observe_flag(EOF_REACHED, "eof-reached");
    player.observe_double(SPEED, "speed");
    player.observe_double(ESTIMATED_FPS, "estimated-vf-fps");
    let player = Rc::new(player);
    SHARED.with(|shared| shared.replace(Some((options.to_owned(), opengl, generation, player.clone()))));
    Some(player)
}

impl MpvStream {
    /// Stops the video and lets go of the player, which stays alive for the next video. Nothing plays after this.
    pub fn stop(&self) {
        for timer in [self.imp().crop_timer.take(), self.imp().position_timer.take(), self.imp().resize_timer.take()].into_iter().flatten() {
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
        let backdrop_changed = imp.framing.get().backdrop != framing.backdrop;
        imp.framing.set(framing);
        self.sync_gl_backdrop();
        if backdrop_changed {
            self.update_backdrop();
        }
        self.invalidate_contents();
        imp.crop.set(None);
        imp.crop_candidate.set(None);
        self.apply_framing_filters();
        imp.crop_settled.set(0);
        imp.crop_looked_at.set(None);
        self.schedule_crop_check();
    }

    /// Looks for where to crop after the wait the last looks call for, in place of any look that is waiting. Nothing is looked
    /// for when the framing does not crop.
    fn schedule_crop_check(&self) {
        let imp = self.imp();
        if let Some(timer) = imp.crop_timer.take() {
            timer.remove();
        }
        if imp.framing.get().crop.is_none() {
            return;
        }
        let weak = self.downgrade();
        let timer = glib::timeout_add_local_once(crop_delay(imp.crop_settled.get()), move || {
            if let Some(stream) = weak.upgrade() {
                stream.imp().crop_timer.take();
                stream.look_for_crop();
            }
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
        player.set_framing_filters(detector, crop);
    }

    /// Looks for where to crop: asks the cropdetect filter, or measures a frame decoded from the file. The next look is set when
    /// this one is done. A paused video is the same frame, so it is looked at only when it has moved since the last look.
    fn look_for_crop(&self) {
        let imp = self.imp();
        let Some(player) = imp.player.borrow().clone() else { return };
        let Some(method) = imp.framing.get().crop else { return };
        let position = imp.position.get();
        if !imp.loaded.get() || (!self.is_playing() && imp.crop_looked_at.get() == Some(position)) || imp.crop_busy.get() {
            self.schedule_crop_check();
            return;
        }
        imp.crop_looked_at.set(Some(position));
        match method {
            // The answer comes back as an event.
            CropMethod::CropDetect => player.request_detected_crop(),
            CropMethod::Edges | CropMethod::Static => {
                let Some((width, height)) = player.source_size() else {
                    self.schedule_crop_check();
                    return;
                };
                // A measurement takes a moment, so the next look waits for it.
                imp.crop_busy.set(true);
                let path = imp.path.borrow().clone();
                let seconds = position as f64 / MICROS;
                let weak = self.downgrade();
                glib::spawn_future_local(async move {
                    let rect = gio::spawn_blocking(move || measure_frames(method, &path, seconds, width, height)).await.ok().flatten();
                    let Some(stream) = weak.upgrade() else { return };
                    stream.imp().crop_busy.set(false);
                    stream.crop_measured(rect);
                });
            }
        }
    }

    /// Takes what a measurement of a frame found, if it found anything: crops to it if the measurement before agrees, so a
    /// frame that is dark for a moment does not crop the video.
    fn crop_measured(&self, rect: Option<CropRect>) {
        let imp = self.imp();
        if let Some(rect) = rect {
            let before = imp.crop_candidate.replace(Some(rect));
            if before.is_some_and(|before| framing::agree(before, rect)) {
                let size = imp.player.borrow().as_ref().and_then(|player| player.source_size());
                self.settle_crop(self.use_crop(rect, size));
            } else {
                imp.crop_settled.set(0);
            }
        }
        self.schedule_crop_check();
    }

    /// Takes what the cropdetect filter found, if it had found anything, and sets the next look.
    fn crop_detected(&self, rect: Option<CropRect>) {
        let imp = self.imp();
        if imp.framing.get().crop != Some(CropMethod::CropDetect) {
            return;
        }
        // Until the file has loaded, what the filter found is about the file before.
        if let Some(rect) = rect.filter(|_| imp.loaded.get()) {
            let size = imp.player.borrow().as_ref().and_then(|player| player.source_size());
            self.settle_crop(self.use_crop(rect, size));
        }
        self.schedule_crop_check();
    }

    /// Counts a look that found the crop as it was, which lengthens the wait for the next, or starts the wait over when the crop
    /// `changed`.
    fn settle_crop(&self, changed: bool) {
        let settled = &self.imp().crop_settled;
        settled.set(if changed { 0 } else { settled.get() + 1 });
    }

    /// Crops to `rect`, unless that is the video as it is or little different from the crop in use. A rectangle that covers
    /// the whole video takes the crop off. Whether the crop changed.
    fn use_crop(&self, rect: CropRect, source: Option<(i32, i32)>) -> bool {
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
        differs
    }

    /// Calls `callback` once the first frame is shown, from when the stream covers everything it is drawn into.
    pub fn connect_first_frame<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("first-frame", false, move |values| {
            let stream = values[0].get::<MpvStream>().expect("stream argument");
            callback(&stream);
            None
        })
    }

    /// Whether a frame has been shown yet.
    pub fn has_frame(&self) -> bool {
        self.imp().frame.borrow().is_some()
    }

    /// Calls `callback` before each seek is sent to the player.
    pub fn connect_seek_requested<F: Fn(&Self) + 'static>(&self, callback: F) -> glib::SignalHandlerId {
        self.connect_local("seek-requested", false, move |values| {
            let stream = values[0].get::<MpvStream>().expect("stream argument");
            callback(&stream);
            None
        })
    }

    /// Sets the volume as a fraction of full, which can go above 1 where the stream's own volume stops.
    pub fn set_level(&self, level: f64) {
        let imp = self.imp();
        imp.level.set(Some(level));
        if let Some(player) = imp.player.borrow().as_ref() {
            player.set_double("volume", level * 100.0);
        }
    }

    /// Plays at `speed` times normal speed.
    pub fn set_speed(&self, speed: f64) {
        if let Some(player) = self.imp().player.borrow().as_ref() {
            self.imp().speed.set(Some(speed));
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
        self.imp().player.borrow().as_ref()?;
        self.imp().speed.get()
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
        self.imp().player.borrow().as_ref()?;
        self.imp().container_fps.get().or(self.imp().estimated_fps.get())
    }

    /// Handles the events waiting from libmpv.
    fn handle_events(&self) {
        loop {
            let event = match self.imp().player.borrow().as_ref() {
                Some(player) => player.next_event(),
                None => return,
            };
            let Some(event) = event else { break };
            self.handle_event(event);
        }
        // Rendering waits while events do, so the render thread is told these have all been read.
        if let Some(player) = self.imp().player.borrow().as_ref() {
            player.wake_renderer();
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
                if let Some(player) = imp.player.borrow().as_ref() {
                    imp.container_fps.set(player.get_double("container-fps"));
                }
                self.apply_rotation();
                self.update_frame_size();
                if !self.is_prepared() {
                    self.stream_prepared(has_audio, true, true, duration);
                }
                if let Some(player) = imp.player.borrow().as_ref() {
                    player.set_string("loop-file", if self.is_loop() { "inf" } else { "no" });
                    player.set_flag("mute", self.is_muted());
                    player.set_double("volume", imp.level.get().unwrap_or_else(|| self.volume()) * 100.0);
                    player.set_flag("pause", !self.is_playing());
                }
            }
            Event::Crop(found) => self.crop_detected(found),
            // The speed stays across files.
            Event::Double(SPEED, speed) => imp.speed.set(speed),
            // Until its own file has loaded, what the player reports is about the file before.
            _ if !imp.loaded.get() => {}
            Event::VideoReconfig => {
                self.apply_rotation();
                self.update_frame_size();
            }
            Event::PlaybackRestart => {
                // The frames before it were from before the seek.
                imp.backdrop_stale.set(true);
                if imp.seeking.replace(false) {
                    self.report_position(true);
                    self.seek_success();
                }
            }
            Event::Double(ESTIMATED_FPS, fps) => imp.estimated_fps.set(fps),
            Event::Double(DURATION, Some(seconds)) => imp.duration.set((seconds * MICROS) as i64),
            Event::Double(TIME_POS, Some(seconds)) => {
                imp.position.set((seconds * MICROS) as i64);
                self.report_position(false);
            }
            Event::Flag(EOF_REACHED, Some(true)) => {
                if self.is_prepared() && !self.is_ended() {
                    // The end is reported at the position it reached.
                    self.report_position(true);
                    self.stream_ended();
                }
            }
            Event::EndFile { error: Some(message) } => {
                self.set_error(glib::Error::new(gtk::glib::FileError::Failed, &message));
            }
            _ => {}
        }
    }

    /// Tells GTK the position mpv last gave, at most once an interval, as it comes with every frame and each report updates the
    /// controls. A position held back is reported when the interval is up. `now` reports it at once, as a seek, a pause or the
    /// end needs.
    fn report_position(&self, now: bool) {
        let imp = self.imp();
        if !self.is_prepared() {
            return;
        }
        let wait = imp.position_at.get().map(|at| POSITION_INTERVAL.saturating_sub(at.elapsed()));
        if now || wait.is_none_or(|wait| wait.is_zero()) {
            if let Some(timer) = imp.position_timer.take() {
                timer.remove();
            }
            imp.position_at.set(Some(std::time::Instant::now()));
            self.update(imp.position.get());
        } else if imp.position_timer.borrow().is_none() {
            let weak = self.downgrade();
            let timer = glib::timeout_add_local_once(wait.unwrap_or_default(), move || {
                if let Some(stream) = weak.upgrade() {
                    stream.imp().position_timer.take();
                    stream.report_position(true);
                }
            });
            imp.position_timer.replace(Some(timer));
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

    /// Sets the display's scale, so frames are rendered at device pixels. The next snapshot renders at the new size.
    pub fn set_scale(&self, scale: f64) {
        if self.imp().scale.replace(scale) != scale {
            self.sync_gl_backdrop();
            self.invalidate_contents();
        }
    }

    /// With OpenGL, has libmpv draw the backdrop into frames of the whole area, instead of it being made here from the frame.
    fn sync_gl_backdrop(&self) {
        let imp = self.imp();
        let Some(player) = imp.player.borrow().clone().filter(|player| player.uses_gl()) else { return };
        let backdrop = imp.framing.get().backdrop;
        player.set_gl_backdrop(backdrop, imp.scale.get().max(1.0));
        imp.frame_fills.set(backdrop.is_some());
    }

    /// Has the picture rendered at the size it is shown at in the area `width` by `height`, in device pixels (or at its own
    /// size, if it is small), when that has changed. Mpv draws its OSD at the size it renders, so rendering at the video's size
    /// and scaling it up would blur the text; the OSD sits in the picture's corner.
    fn fit_render(&self, width: f64, height: f64) {
        let imp = self.imp();
        let scale = imp.scale.get().max(1.0);
        let area_changed = imp.area.replace((width, height)) != (width, height);
        // The frame is the picture alone, as big as fits the area, so no pixels are rendered or uploaded for the bars round it.
        let (width, height) = match imp.frame_size.get() {
            _ if imp.frame_fills.get() => (width, height),
            (video_width, video_height) if video_width > 0 && video_height > 0 => {
                let (_, _, fitted_width, fitted_height) = framing::fitted(width, height, f64::from(video_width), f64::from(video_height));
                (fitted_width, fitted_height)
            }
            _ => (width, height),
        };
        let (width, height) = capped((width * scale).round().max(1.0) as i32, (height * scale).round().max(1.0) as i32);
        // A small video is not rendered bigger than it is.
        let (width, height) = match imp.frame_size.get() {
            _ if imp.frame_fills.get() => (width, height),
            (own_width, own_height) if own_width > 0 && own_width <= OWN_SIZE_MAX_WIDTH && own_width < width => (own_width, own_height),
            _ => (width, height),
        };
        if width <= 0 || height <= 0 || !self.is_prepared() {
            return;
        }
        if imp.render_size.get() == (width, height) {
            // Back at the size rendered, so there is nothing to wait for.
            if let Some(timer) = imp.resize_timer.take() {
                timer.remove();
            }
            // The frame is as it was, but the strips round it are not.
            if area_changed {
                self.update_backdrop();
            }
            return;
        }
        imp.wanted_size.set((width, height));
        if area_changed {
            self.update_backdrop();
        }
        // Rendering with OpenGL costs too little to wait for the size to settle.
        let uses_gl = imp.player.borrow().as_ref().is_some_and(|player| player.uses_gl());
        let settling = !uses_gl && imp.frame.borrow().is_some() && imp.resized_at.get().is_some_and(|at| at.elapsed() < RESIZE_SETTLE);
        if !settling {
            self.render_wanted();
            return;
        }
        if let Some(timer) = imp.resize_timer.take() {
            timer.remove();
        }
        let weak = self.downgrade();
        let timer = glib::timeout_add_local_once(RESIZE_SETTLE, move || {
            if let Some(stream) = weak.upgrade() {
                stream.imp().resize_timer.take();
                stream.render_wanted();
            }
        });
        imp.resize_timer.replace(Some(timer));
    }

    /// Renders at the size wanted, now.
    fn render_wanted(&self) {
        let imp = self.imp();
        imp.render_size.set(imp.wanted_size.get());
        imp.resized_at.set(Some(std::time::Instant::now()));
        imp.backdrop_stale.set(true);
        if let Some(player) = imp.player.borrow().as_ref() {
            player.render_at(imp.render_size.get().0, imp.render_size.get().1);
        }
    }

    /// Makes the backdrop from the latest frame, on a worker thread, and draws it when it is done. Only one is made at a time. With
    /// none to show yet it is made now, so the strips round the first frame are not left bare.
    fn update_backdrop(&self) {
        let imp = self.imp();
        let Some(backdrop) = imp.framing.get().backdrop else {
            imp.backdrop.borrow_mut().clear();
            return;
        };
        let Some(frame) = imp.last_frame.borrow().clone() else { return };
        let area = imp.area.get();
        if area.0 <= 0.0 || area.1 <= 0.0 || frame.1 <= 0 || frame.2 <= 0 {
            return;
        }
        if imp.backdrop_busy.get() {
            imp.backdrop_again.set(true);
            return;
        }
        imp.backdrop_stale.set(false);
        let scale = imp.scale.get().max(1.0);
        let mut scratch = imp.blur_scratch.take();
        if imp.backdrop.borrow().is_empty() {
            let pieces = make_backdrop(&frame, area, scale, backdrop, &mut scratch);
            imp.blur_scratch.replace(scratch);
            self.backdrop_made(pieces);
            return;
        }
        imp.backdrop_busy.set(true);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let made = gio::spawn_blocking(move || {
                let pieces = make_backdrop(&frame, area, scale, backdrop, &mut scratch);
                (pieces, scratch)
            })
            .await
            .ok();
            let Some(stream) = weak.upgrade() else { return };
            let imp = stream.imp();
            imp.backdrop_busy.set(false);
            if let Some((pieces, scratch)) = made {
                imp.blur_scratch.replace(scratch);
                // It may have been turned off meanwhile.
                if imp.framing.get().backdrop.is_some() {
                    stream.backdrop_made(pieces);
                    stream.invalidate_contents();
                }
            }
            if imp.backdrop_again.replace(false) {
                stream.update_backdrop();
            }
        });
    }

    /// Makes textures of the pieces of a backdrop, to draw from now on.
    fn backdrop_made(&self, pieces: Vec<(Vec<u8>, usize, usize)>) {
        let imp = self.imp();
        let textures = pieces.into_iter().map(|(pixels, width, height)| {
            let bytes = glib::Bytes::from_owned(pixels);
            gdk::MemoryTexture::new(width as i32, height as i32, gdk::MemoryFormat::B8g8r8x8, &bytes, width * 4).upcast()
        });
        imp.backdrop.replace(textures.collect());
        imp.backdrop_at.set(Some(std::time::Instant::now()));
    }

    /// Shows a frame the render thread rendered for the player with `player_id`, unless that is not the player any more.
    fn frame_rendered(&self, player_id: u64, frame: RenderedFrame) {
        let imp = self.imp();
        if imp.player.borrow().as_ref().map(|player| player.id()) != Some(player_id) {
            return;
        }
        let RenderedFrame { image, width, height, ticket } = frame;
        let texture: gdk::Texture = match image {
            FrameImage::Memory(pixels) => {
                let bytes = glib::Bytes::from_owned(pixels);
                let texture = gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::B8g8r8x8, &bytes, width as usize * 4);
                imp.last_frame.replace(Some((bytes, width, height)));
                texture.upcast()
            }
            FrameImage::Gl(frame) => frame.into_texture(),
        };
        // The render thread may render the next frame now.
        drop(ticket);
        let backdrop_due = !imp.backdrop_busy.get() && imp.backdrop_at.get().is_none_or(|at| at.elapsed() >= BACKDROP_INTERVAL);
        if imp.framing.get().backdrop.is_some() && (imp.backdrop_stale.get() || backdrop_due) {
            self.update_backdrop();
        }
        let first = imp.frame.replace(Some(texture)).is_none();
        self.invalidate_contents();
        if first {
            self.emit_by_name::<()>("first-frame", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backdrop_is_made_for_each_bare_strip_at_an_eighth_of_its_size() {
        let frame = (glib::Bytes::from_owned(vec![200u8; 100 * 100 * 4]), 100, 100);
        let backdrop = Backdrop { zoom: 1.0, blur: 8, dim: 0.5 };
        let mut scratch = Vec::new();
        let pieces = make_backdrop(&frame, (200.0, 100.0), 1.0, backdrop, &mut scratch);
        let sizes: Vec<(usize, usize)> = pieces.iter().map(|(_, width, height)| (*width, *height)).collect();
        assert_eq!(sizes, [(7, 13), (7, 13)]);
        assert!(pieces.iter().all(|(pixels, width, height)| pixels.len() == width * height * 4 && pixels[0] == 100));
        // A picture that fills the area leaves no strips.
        assert!(make_backdrop(&frame, (100.0, 100.0), 1.0, backdrop, &mut scratch).is_empty());
    }

    #[test]
    fn the_wait_for_the_crop_doubles_up_to_a_longest() {
        let seconds: Vec<u64> = (0..7).map(|settled| crop_delay(settled).as_secs()).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 8, 8, 8]);
    }
}
