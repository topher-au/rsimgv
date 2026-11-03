//! A small, safe wrapper over libmpv: a player that loads files, takes commands and properties, reports
//! events, and renders frames in software into memory for GTK to show. Each player listens for mpv JSON IPC on
//! `/tmp/rsimgv-mpv-<pid in hex>`. See `stream` for the GTK side.

pub mod framing;
mod stream;
mod sys;

pub use framing::{CropRect, Framing};
pub use stream::MpvStream;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::io::{BufRead, BufReader, Write};
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk::gio;
use gtk::gio::prelude::*;

use crate::orientation::Orientation;

/// An event from the player, as `Player::next_event` reports it.
#[derive(Debug)]
pub enum Event {
    FileLoaded,
    /// The file ended: at its end, or by an error with its message.
    EndFile { error: Option<String> },
    PlaybackRestart,
    VideoReconfig,
    /// An observed property changed, with the id it was observed under; `None` when it has no value.
    Double(u64, Option<f64>),
    Flag(u64, Option<bool>),
    /// What the cropdetect filter had found when `Player::request_detected_crop` asked, if it had found anything.
    Crop(Option<CropRect>),
    Shutdown,
    Other,
}

/// The id the reply to `Player::request_detected_crop` comes back under, apart from the ids of observed properties.
const REPLY_CROP: u64 = 1 << 32;

/// The id the output's rotation is observed under, and the one the reply that confirms it comes back under. The player
/// handles both itself, as a frame may not be rendered until the rotation is known to be none.
const OBSERVE_ROTATION: u64 = 1 << 33;
const REPLY_ROTATION: u64 = 1 << 34;

/// How many times the player tries to turn rotation back off, after a check finds the output rotated, before it gives up on
/// rendering until the video is reconfigured.
const ROTATION_RETRIES: u32 = 2;

/// Whether frames may be rendered, kept by the player and read by whatever renders. A frame is rendered only when no event from
/// libmpv is waiting, so that a reconfiguration that came before it has been seen, and when the output is known not to be
/// rotated: libmpv's software renderer aborts the whole process on a rotated frame (see `Player::new`).
#[derive(Default)]
struct RenderGate {
    /// Set by libmpv's wakeup, which comes with every event, and cleared once the events waiting have all been read.
    events_pending: AtomicBool,
    /// Cleared when the video is reconfigured or its rotation changes, until a check made since then finds no rotation.
    rotation_unconfirmed: AtomicBool,
}

impl RenderGate {
    /// Whether a frame may be rendered now.
    fn open(&self) -> bool {
        !self.events_pending.load(Ordering::Acquire) && !self.rotation_unconfirmed.load(Ordering::Acquire)
    }
}

/// A callback libmpv calls from its own threads.
type Wakeup = Box<dyn Fn() + Send + Sync>;

unsafe extern "C" fn call_wakeup(data: *mut c_void) {
    // SAFETY: `data` is the `Wakeup` boxed in `set_wakeup` or `set_render_update`, which outlives the player.
    let callback = unsafe { &*(data as *const Wakeup) };
    callback();
}

/// The text of a libmpv error code.
fn error_text(code: c_int) -> String {
    // SAFETY: libmpv returns a static string for every code.
    unsafe { CStr::from_ptr(sys::mpv_error_string(code)) }.to_string_lossy().into_owned()
}

fn check(code: c_int) -> Result<(), String> {
    if code < 0 { Err(error_text(code)) } else { Ok(()) }
}

thread_local! {
    /// Bumped when mpv.conf changes, so the next player is made with the new file.
    static CONFIG_GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Watches the config directory, for as long as the app runs.
    static CONFIG_MONITOR: RefCell<Option<gio::FileMonitor>> = const { RefCell::new(None) };
    /// The options mpv.conf had when it was last read, to find what changed.
    static LAST_CONFIG: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
}

/// mpv's config and scripts live in `rsimgv/mpv` under the user's config directory, kept with rsimgv's settings.
pub fn config_dir() -> std::path::PathBuf {
    gtk::glib::user_config_dir().join("rsimgv").join("mpv")
}

/// How many times mpv.conf has changed since the app started.
pub fn config_generation() -> u64 {
    CONFIG_GENERATION.with(Cell::get)
}

/// Calls `on_change` on the main thread when mpv.conf in the config directory changes. Players made after that
/// read the new file, so `on_change` should play the current video again.
pub fn watch_config(on_change: impl Fn() + 'static) {
    let dir = config_dir();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("rsimgv: mpv: could not create {}: {err}", dir.display());
        return;
    }
    let monitor = match gio::File::for_path(&dir).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
        Ok(monitor) => monitor,
        Err(err) => {
            eprintln!("rsimgv: mpv: could not watch {}: {err}", dir.display());
            return;
        }
    };
    LAST_CONFIG.with(|last| last.replace(read_config()));
    monitor.connect_changed(move |_, file, _, event| {
        // A save can arrive as several events; the hint that writing is done comes once, after the last of them.
        let done = matches!(
            event,
            gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::Deleted
        );
        if !done || file.basename().as_deref() != Some(Path::new("mpv.conf")) {
            return;
        }
        let new = read_config();
        let (changed, removed) = LAST_CONFIG.with(|last| {
            let last = last.borrow();
            let changed: Vec<(String, String)> =
                new.iter().filter(|(name, value)| last.get(*name) != Some(value)).map(|(n, v)| (n.clone(), v.clone())).collect();
            let removed = last.keys().any(|name| !new.contains_key(name));
            (changed, removed)
        });
        LAST_CONFIG.with(|last| last.replace(new));
        if changed.is_empty() && !removed {
            return;
        }
        // Options that were removed have no value to set back, so only a new player can drop them.
        if !removed && set_properties(&changed).is_ok() {
            return;
        }
        CONFIG_GENERATION.with(|generation| generation.set(generation.get() + 1));
        on_change();
    });
    CONFIG_MONITOR.with(|slot| slot.replace(Some(monitor)));
}

/// Reads mpv.conf in the config directory, or nothing if there is none.
fn read_config() -> BTreeMap<String, String> {
    std::fs::read_to_string(config_dir().join("mpv.conf")).map(|text| parse_config(&text)).unwrap_or_default()
}

/// The options in an mpv config file, by name, with their values. Blank lines, comments, profile headers and
/// `include` lines are skipped; a bare `name` is `yes`, and `no-name` is `name=no`.
pub fn parse_config(text: &str) -> BTreeMap<String, String> {
    let mut options = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') || line.starts_with("include") {
            continue;
        }
        let (name, value) = match line.split_once('=') {
            Some((name, value)) => (name.trim(), value.trim()),
            None => match line.strip_prefix("no-") {
                Some(name) => (name, "no"),
                None => (line, "yes"),
            },
        };
        let value = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')).unwrap_or(value);
        options.insert(name.to_owned(), value.to_owned());
    }
    options
}

/// Sets each option on the running player through its JSON IPC socket, with mpv's `set_property` command. Fails
/// if there is no player to set them on, or if mpv does not accept one of them.
pub fn set_properties(changes: &[(String, String)]) -> Result<(), String> {
    let mut socket = UnixStream::connect(ipc_socket_path()).map_err(|err| err.to_string())?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).map_err(|err| err.to_string())?;
    let mut replies = BufReader::new(socket.try_clone().map_err(|err| err.to_string())?);
    for (name, value) in changes {
        let request = serde_json::json!({"command": ["set_property", name, value]});
        writeln!(socket, "{request}").map_err(|err| err.to_string())?;
        // Replies come one per line; anything else on the socket is skipped.
        loop {
            let mut line = String::new();
            if replies.read_line(&mut line).map_err(|err| err.to_string())? == 0 {
                return Err("mpv closed the socket".to_owned());
            }
            let Ok(reply) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let Some(error) = reply.get("error").and_then(serde_json::Value::as_str) else { continue };
            if error != "success" {
                return Err(format!("could not set {name}: {error}"));
            }
            break;
        }
    }
    Ok(())
}

/// The start of each player's mpv IPC socket name, which is followed by the process id in lowercase hex.
const SOCKET_PREFIX: &str = "rsimgv-mpv-";

/// Where the player's mpv IPC socket is: `/tmp/rsimgv-mpv-<pid>`, with the process id in lowercase hex.
pub fn ipc_socket_path() -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/{SOCKET_PREFIX}{:x}", std::process::id()))
}

/// Removes the IPC sockets left by players that are no longer running, such as after a crash. Sockets of running
/// processes, including other rsimgv windows, are kept.
pub fn remove_stale_sockets() {
    let Ok(entries) = std::fs::read_dir("/tmp") else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_str().and_then(|name| name.strip_prefix(SOCKET_PREFIX)).and_then(|hex| u32::from_str_radix(hex, 16).ok());
        let stale = pid.is_some_and(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists());
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Options that would take the video out of rsimgv's window, so they are never applied from the user's options.
const EMBEDDING_OPTIONS: &[&str] = &["vo", "wid", "gpu-context", "fs", "fullscreen"];

/// mpv command line options as option names and values. Each option starts with `--` after a space or at the
/// start, and runs to the next one, so a value may hold spaces. `--no-name` is `name=no`, and a bare `--name`
/// is `name=yes`.
pub fn parse_options(text: &str) -> Vec<(String, String)> {
    let mut starts: Vec<usize> = text
        .match_indices("--")
        .map(|(index, _)| index)
        .filter(|&index| index == 0 || text[..index].ends_with(char::is_whitespace))
        .collect();
    starts.dedup();
    let mut options = Vec::new();
    for (position, &start) in starts.iter().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(text.len());
        let option = text[start + 2..end].trim();
        if option.is_empty() {
            continue;
        }
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => match option.strip_prefix("no-") {
                Some(name) => (name.to_owned(), "no".to_owned()),
                None => (option.to_owned(), "yes".to_owned()),
            },
        };
        options.push((name, value));
    }
    options
}

/// The label of the filter that turns a rotated video upright, so it can be replaced on its own.
const UPRIGHT_FILTER_LABEL: &str = "@rsimgv-upright";

/// The label of the filter that turns and mirrors the video as the user asked.
const ORIENTATION_FILTER_LABEL: &str = "@rsimgv-orientation";

/// The filter that turns upright a video stored `rotation` degrees clockwise of how it should be shown, if it needs
/// one.
fn upright_filter(rotation: i64) -> Option<String> {
    let graph = match rotation.rem_euclid(360) {
        0 => return None,
        90 => "transpose=clock".to_owned(),
        180 => "hflip,vflip".to_owned(),
        270 => "transpose=cclock".to_owned(),
        angle => {
            let radians = (angle as f64).to_radians();
            format!("rotate={radians:.6}:ow=rotw({radians:.6}):oh=roth({radians:.6})")
        }
    };
    Some(format!("{UPRIGHT_FILTER_LABEL}:lavfi=[{graph}]"))
}

/// The filters that go first in the chain, as one list for `vf pre`, which puts them in the order given: the upright filter, then
/// the detector, then the crop, so the detector and the crop see the video as it is shown. `None` when there are none.
fn front_filters(rotation: i64, detector: bool, crop: Option<CropRect>) -> Option<String> {
    let filters: Vec<String> = [upright_filter(rotation), detector.then(framing::detector_filter), crop.map(framing::crop_filter)].into_iter().flatten().collect();
    (!filters.is_empty()).then(|| filters.join(","))
}

/// A captured frame: its size, the bytes from one row to the next, and its pixels.
pub struct Frame {
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub pixels: Vec<u8>,
}

/// The frame in a `screenshot-raw` result: a map with `w`, `h`, `stride` and the `data` bytes.
///
/// # Safety
/// `node` must be a result libmpv filled in and has not freed.
unsafe fn read_frame(node: &sys::mpv_node) -> Result<Frame, String> {
    if node.format != sys::MPV_FORMAT_NODE_MAP {
        return Err("mpv did not return a frame".to_owned());
    }
    let (mut width, mut height, mut stride, mut pixels) = (None, None, None, None);
    // SAFETY: a node map holds `num` keys and values, valid while the node is.
    unsafe {
        let list = &*node.u.list;
        for index in 0..list.num.max(0) as usize {
            let key = CStr::from_ptr(*list.keys.add(index)).to_string_lossy();
            let value = &*list.values.add(index);
            match (key.as_ref(), value.format) {
                ("w", sys::MPV_FORMAT_INT64) => width = Some(value.u.int64),
                ("h", sys::MPV_FORMAT_INT64) => height = Some(value.u.int64),
                ("stride", sys::MPV_FORMAT_INT64) => stride = Some(value.u.int64),
                ("data", sys::MPV_FORMAT_BYTE_ARRAY) => {
                    let bytes = &*value.u.ba;
                    pixels = Some(std::slice::from_raw_parts(bytes.data as *const u8, bytes.size).to_vec());
                }
                _ => {}
            }
        }
    }
    match (width, height, stride, pixels) {
        (Some(width), Some(height), Some(stride), Some(pixels)) if width > 0 && height > 0 && pixels.len() >= (stride * height) as usize => {
            Ok(Frame { width: width as i32, height: height as i32, stride: stride as usize, pixels })
        }
        _ => Err("mpv returned an incomplete frame".to_owned()),
    }
}

/// The crop in the metadata the cropdetect filter reports: a map of `lavfi.cropdetect.w`, `.h`, `.x` and `.y`, as text.
///
/// # Safety
/// `node` must be a result libmpv filled in and has not freed.
unsafe fn read_crop(node: &sys::mpv_node) -> Option<CropRect> {
    if node.format != sys::MPV_FORMAT_NODE_MAP {
        return None;
    }
    let (mut width, mut height, mut left, mut top) = (None, None, None, None);
    // SAFETY: a node map holds `num` keys and values, valid while the node is.
    unsafe {
        let list = &*node.u.list;
        for index in 0..list.num.max(0) as usize {
            let key = CStr::from_ptr(*list.keys.add(index)).to_string_lossy();
            let value = &*list.values.add(index);
            if value.format != sys::MPV_FORMAT_STRING || value.u.string.is_null() {
                continue;
            }
            let number = CStr::from_ptr(value.u.string).to_string_lossy().trim().parse::<i32>().ok();
            match key.as_ref() {
                "lavfi.cropdetect.w" => width = number,
                "lavfi.cropdetect.h" => height = number,
                "lavfi.cropdetect.x" => left = number,
                "lavfi.cropdetect.y" => top = number,
                _ => {}
            }
        }
    }
    Some(CropRect { width: width?, height: height?, left: left?, top: top? }).filter(|rect| rect.width > 0 && rect.height > 0)
}

/// A libmpv player with a software render context.
pub struct Player {
    handle: *mut sys::mpv_handle,
    render: *mut sys::mpv_render_context,
    /// The callbacks handed to libmpv, freed once it is gone.
    wakeup: Option<*mut Wakeup>,
    render_update: Option<*mut Wakeup>,
    /// The rotation, in degrees, that the upright filter now undoes.
    rotation: Cell<i64>,
    /// The turn and mirror the user gave the video, which the orientation filter applies.
    orientation: Cell<Orientation>,
    /// The filter that `set_interpolation_filter` last put in the chain.
    interpolation: std::cell::RefCell<Option<String>>,
    /// The crop filter and the detector that finds where to crop that `set_framing_filters` put in the chain.
    crop: Cell<Option<CropRect>>,
    detector: Cell<bool>,
    /// Whether frames may be rendered, and how many times the output has been found rotated since the video was reconfigured.
    gate: Arc<RenderGate>,
    rotation_checks: Cell<u32>,
}

impl Player {
    /// A player set up to play without a window of its own, rendering into memory, with the mpv command line
    /// options in `extra` applied over rsimgv's own. Options that would give it a window are left out.
    pub fn new(extra: &str) -> Result<Self, String> {
        // SAFETY: plain libmpv calls on a handle this function owns until it is returned.
        unsafe {
            // libmpv refuses to start unless numbers are formatted the C way, and GTK sets the locale from the
            // environment at startup. Only the number format changes; text and dates stay localised.
            sys::setlocale(sys::LC_NUMERIC, c"C".as_ptr());
            let handle = sys::mpv_create();
            if handle.is_null() {
                return Err("could not create the mpv player".to_owned());
            }
            let mut player = Self { handle, render: std::ptr::null_mut(), wakeup: None, render_update: None, rotation: Cell::new(0), orientation: Cell::new(Orientation::default()), interpolation: Default::default(), crop: Cell::new(None), detector: Cell::new(false), gate: Arc::default(), rotation_checks: Cell::new(0) };
            for (name, value) in [
                ("vo", "libmpv"),
                ("hwdec", "auto-safe"),
                ("keep-open", "yes"),
                ("idle", "yes"),
                ("terminal", "no"),
                ("input-default-bindings", "no"),
                ("osc", "no"),
                ("ytdl", "no"),
                // The OSD keeps its size in device pixels, so resizing the window does not change it.
                ("osd-scale-by-window", "no"),
                // The volume goes up to 200%; mpv stops at 130% unless it is told otherwise.
                ("volume-max", "200"),
                // Rendering must not wait for a frame's display time on the main thread, which libmpv does for up to
                // this long a frame. A frame is shown when it is drawn.
                ("video-timing-offset", "0"),
            ] {
                player.set_option(name, value)?;
            }
            // Other programs can drive the player through mpv's JSON IPC on this socket, unless the user's
            // options name another.
            let socket = ipc_socket_path();
            if let Err(err) = player.set_option("input-ipc-server", &socket.to_string_lossy()) {
                eprintln!("rsimgv: mpv: could not open the IPC socket {}: {err}", socket.display());
            }
            player.set_option("config-dir", &config_dir().to_string_lossy())?;
            for (name, value) in parse_options(extra) {
                if EMBEDDING_OPTIONS.contains(&name.as_str()) {
                    continue;
                }
                if let Err(err) = player.set_option(&name, &value) {
                    eprintln!("rsimgv: mpv: could not set --{name}={value}: {err}");
                }
            }
            check(sys::mpv_initialize(handle))?;
            // libmpv's output claims it can rotate, so frames reach it still marked as rotated, but its software
            // renderer cannot: a quarter turn makes it crop past the frame, which aborts the whole process. Set
            // after initializing, so neither the user's options nor mpv.conf can undo it. `set_rotation` turns the
            // video upright with a filter instead.
            player.set_option("video-rotate", "no")?;
            // If rotation is asked for anyway, through IPC or a changed mpv.conf, rendering waits until it is off again.
            player.observe(OBSERVE_ROTATION, "video-out-params/rotate", sys::MPV_FORMAT_INT64);

            let api = CString::new("sw").expect("no nul");
            let mut params = [
                sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
                sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
            ];
            check(sys::mpv_render_context_create(&mut player.render, handle, params.as_mut_ptr()))?;
            Ok(player)
        }
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let (name, value) = (CString::new(name).map_err(|e| e.to_string())?, CString::new(value).map_err(|e| e.to_string())?);
        // SAFETY: the handle is live and the strings outlive the call.
        check(unsafe { sys::mpv_set_option_string(self.handle, name.as_ptr(), value.as_ptr()) })
    }

    /// Runs a command, such as `["loadfile", path]`, without waiting for it.
    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let args: Vec<CString> = args.iter().map(|arg| CString::new(*arg).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        let mut pointers: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).chain(std::iter::once(std::ptr::null())).collect();
        // SAFETY: a null-terminated array of strings that outlive the call.
        check(unsafe { sys::mpv_command_async(self.handle, 0, pointers.as_mut_ptr()) })
    }

    /// Turns upright a video stored `rotation` degrees clockwise of how it should be shown.
    pub fn set_rotation(&self, rotation: i64) {
        if self.rotation.replace(rotation) == rotation {
            return;
        }
        self.rebuild_front_filters();
        // The frame shown already passed the filters, and a paused video would stay that way, so it is decoded again.
        let _ = self.command(&["seek", "0", "relative+exact"]);
    }

    /// Turns and mirrors the video as `orientation` says, after the upright filter, which has made it upright.
    pub fn set_orientation(&self, orientation: Orientation) {
        let before = self.orientation.replace(orientation);
        if before == orientation {
            return;
        }
        if before.filter_graph().is_some() {
            if let Err(err) = self.command(&["vf", "remove", ORIENTATION_FILTER_LABEL]) {
                eprintln!("rsimgv: mpv: could not remove the orientation filter: {err}");
            }
        }
        if let Some(graph) = orientation.filter_graph() {
            if let Err(err) = self.command(&["vf", "add", &format!("{ORIENTATION_FILTER_LABEL}:lavfi=[{graph}]")]) {
                eprintln!("rsimgv: mpv: could not add the orientation filter: {err}");
            }
        }
        // The frame shown already passed the filters, and a paused video would stay that way, so it is decoded again.
        let _ = self.command(&["seek", "0", "relative+exact"]);
    }

    /// Puts `filter`, which is labelled `label`, after the other video filters in place of the one put there before, or
    /// takes that one out when it is `None`. The upright filter and those from the user's options stay. Nothing happens
    /// if it is there already, as changing the chain holds up the video.
    pub fn set_interpolation_filter(&self, label: &str, filter: Option<&str>) -> Result<(), String> {
        if self.interpolation.borrow().as_deref() == filter {
            return Ok(());
        }
        if self.interpolation.borrow().is_some() {
            self.command(&["vf", "remove", label])?;
        }
        if let Some(filter) = filter {
            self.command(&["vf", "add", filter])?;
        }
        self.interpolation.replace(filter.map(str::to_owned));
        Ok(())
    }

    /// Sets the filters that frame the video: the detector that finds the picture, and the crop to `crop`. The upright filter
    /// stays first, so the detector and the crop see the video as it is shown. A change holds up the video, so nothing
    /// happens for what is already in the chain.
    pub fn set_framing_filters(&self, detector: bool, crop: Option<CropRect>) {
        if self.detector.replace(detector) == detector && self.crop.replace(crop) == crop {
            return;
        }
        self.crop.set(crop);
        self.rebuild_front_filters();
        // The frame shown already passed the filters, and a paused video would stay that way, so it is decoded again.
        if self.get_flag("pause") == Some(true) {
            let _ = self.command(&["seek", "0", "relative+exact"]);
        }
    }

    /// Puts the upright filter, the detector and the crop first in the chain, in that order, in place of any before, with one
    /// command to take the old ones out and one to put the new ones in. The filters from the user's options and SVP stay.
    fn rebuild_front_filters(&self) {
        // Removing a label that is not in the chain does nothing.
        let labels = [UPRIGHT_FILTER_LABEL.to_owned(), format!("@{}", framing::DETECTOR_LABEL), format!("@{}", framing::CROP_LABEL)].join(",");
        if let Err(err) = self.command(&["vf", "remove", &labels]) {
            eprintln!("rsimgv: mpv: could not remove {labels}: {err}");
        }
        if let Some(filters) = front_filters(self.rotation.get(), self.detector.get(), self.crop.get()) {
            if let Err(err) = self.command(&["vf", "pre", &filters]) {
                eprintln!("rsimgv: mpv: could not add the filters: {err}");
            }
        }
    }

    /// Asks for what the cropdetect filter last found, which comes back as `Event::Crop`. It is asked for without waiting, as
    /// the answer is the mpv core's to give.
    pub fn request_detected_crop(&self) {
        let Ok(name) = CString::new(format!("vf-metadata/{}", framing::DETECTOR_LABEL)) else { return };
        // SAFETY: the handle is live; libmpv copies the name.
        let code = unsafe { sys::mpv_get_property_async(self.handle, REPLY_CROP, name.as_ptr(), sys::MPV_FORMAT_NODE) };
        if code < 0 {
            eprintln!("rsimgv: mpv: could not ask for the detected crop: {}", error_text(code));
        }
    }

    /// The size of the video as decoded and turned upright, before any crop, if it is known.
    pub fn source_size(&self) -> Option<(i32, i32)> {
        let (width, height) = (self.get_i64("video-params/w")? as i32, self.get_i64("video-params/h")? as i32);
        Some(if self.rotation.get().rem_euclid(180) == 90 { (height, width) } else { (width, height) })
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let name = CString::new(name).ok()?;
        let mut value: c_int = 0;
        // SAFETY: a flag is written into `value`.
        let code = unsafe { sys::mpv_get_property(self.handle, name.as_ptr(), sys::MPV_FORMAT_FLAG, &mut value as *mut c_int as *mut c_void) };
        (code >= 0).then_some(value != 0)
    }

    /// The video's current frame, at its own size, as mpv's `screenshot-raw` command captures it, with
    /// subtitles as they are shown. Its pixels are 4 bytes each, in BGRA order.
    pub fn screenshot(&self) -> Result<Frame, String> {
        let args: Vec<CString> = ["screenshot-raw", "subtitles", "bgra"].iter().map(|arg| CString::new(*arg).expect("no nul")).collect();
        let mut pointers: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).chain(std::iter::once(std::ptr::null())).collect();
        // SAFETY: the node is filled in by libmpv, read while it is valid, and freed once read.
        unsafe {
            let mut node: sys::mpv_node = std::mem::zeroed();
            check(sys::mpv_command_ret(self.handle, pointers.as_mut_ptr(), &mut node))?;
            let frame = read_frame(&node);
            sys::mpv_free_node_contents(&mut node);
            frame
        }
    }

    pub fn set_flag(&self, name: &str, value: bool) {
        let mut flag: c_int = value.into();
        self.set_property(name, sys::MPV_FORMAT_FLAG, &mut flag as *mut c_int as *mut c_void);
    }

    pub fn set_double(&self, name: &str, value: f64) {
        let mut value = value;
        self.set_property(name, sys::MPV_FORMAT_DOUBLE, &mut value as *mut f64 as *mut c_void);
    }

    pub fn set_string(&self, name: &str, value: &str) {
        let Ok(value) = CString::new(value) else { return };
        let mut pointer = value.as_ptr();
        self.set_property(name, sys::MPV_FORMAT_STRING, &mut pointer as *mut *const c_char as *mut c_void);
    }

    fn set_property(&self, name: &str, format: c_int, data: *mut c_void) {
        let Ok(name) = CString::new(name) else { return };
        // SAFETY: `data` points at a value of `format` that libmpv copies before returning.
        let code = unsafe { sys::mpv_set_property_async(self.handle, 0, name.as_ptr(), format, data) };
        if code < 0 {
            eprintln!("rsimgv: mpv: could not set {}: {}", name.to_string_lossy(), error_text(code));
        }
    }

    pub fn get_double(&self, name: &str) -> Option<f64> {
        let name = CString::new(name).ok()?;
        let mut value = 0.0_f64;
        // SAFETY: a double is written into `value`.
        let code = unsafe { sys::mpv_get_property(self.handle, name.as_ptr(), sys::MPV_FORMAT_DOUBLE, &mut value as *mut f64 as *mut c_void) };
        (code >= 0).then_some(value)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let name = CString::new(name).ok()?;
        let mut value = 0_i64;
        // SAFETY: an int64 is written into `value`.
        let code = unsafe { sys::mpv_get_property(self.handle, name.as_ptr(), sys::MPV_FORMAT_INT64, &mut value as *mut i64 as *mut c_void) };
        (code >= 0).then_some(value)
    }

    /// Reports changes to `name` as `Event::Double` with `id`.
    pub fn observe_double(&self, id: u64, name: &str) {
        self.observe(id, name, sys::MPV_FORMAT_DOUBLE);
    }

    /// Reports changes to `name` as `Event::Flag` with `id`.
    pub fn observe_flag(&self, id: u64, name: &str) {
        self.observe(id, name, sys::MPV_FORMAT_FLAG);
    }

    fn observe(&self, id: u64, name: &str, format: c_int) {
        let Ok(name) = CString::new(name) else { return };
        // SAFETY: the handle is live; libmpv copies the name.
        unsafe { sys::mpv_observe_property(self.handle, id, name.as_ptr(), format) };
    }

    /// The next waiting event, or `None` once there are none. Frames are not rendered while there are events to read, so the
    /// caller reads them all.
    pub fn next_event(&self) -> Option<Event> {
        let event = self.read_event().or_else(|| {
            // None are waiting. But one may have come since, with a wakeup that this would clear, so look again.
            self.gate.events_pending.store(false, Ordering::Release);
            let late = self.read_event();
            if late.is_some() {
                self.gate.events_pending.store(true, Ordering::Release);
            }
            late
        })?;
        Some(event)
    }

    /// Asks, without waiting, whether the output is rotated; the reply is `rotation_replied`.
    fn request_rotation(&self) {
        let name = c"video-out-params/rotate";
        // SAFETY: the handle is live; libmpv copies the name.
        let code = unsafe { sys::mpv_get_property_async(self.handle, REPLY_ROTATION, name.as_ptr(), sys::MPV_FORMAT_INT64) };
        if code < 0 {
            eprintln!("rsimgv: mpv: could not ask whether the output is rotated: {}", error_text(code));
            // Without an answer nothing could be confirmed, so rendering stops here.
        }
    }

    /// The video was reconfigured, or its output rotation changed to `rotate`: no frame is rendered until a check made after
    /// this finds the output not rotated. The reply cannot be older than the change, as mpv answers in the order it was asked.
    fn rotation_unconfirmed(&self, rotate: Option<i64>) {
        self.gate.rotation_unconfirmed.store(true, Ordering::Release);
        self.rotation_checks.set(0);
        if rotate.is_some_and(|rotate| rotate != 0) {
            self.set_string("video-rotate", "no");
        }
        self.request_rotation();
    }

    /// The reply to `request_rotation`, `None` when the output has no rotation to report, such as with no video.
    fn rotation_replied(&self, rotate: Option<i64>) {
        if rotate.is_some_and(|rotate| rotate != 0) {
            let checks = self.rotation_checks.get() + 1;
            self.rotation_checks.set(checks);
            if checks > ROTATION_RETRIES {
                eprintln!("rsimgv: mpv: the output stays rotated, which the software renderer cannot draw; not rendering");
                return;
            }
            self.set_string("video-rotate", "no");
            self.request_rotation();
            return;
        }
        self.gate.rotation_unconfirmed.store(false, Ordering::Release);
    }

    fn read_event(&self) -> Option<Event> {
        // SAFETY: the event is valid until the next call, and is read fully before then.
        unsafe {
            let event = &*sys::mpv_wait_event(self.handle, 0.0);
            Some(match event.event_id {
                sys::MPV_EVENT_NONE => return None,
                sys::MPV_EVENT_SHUTDOWN => Event::Shutdown,
                sys::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
                sys::MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
                sys::MPV_EVENT_VIDEO_RECONFIG => {
                    self.rotation_unconfirmed(None);
                    Event::VideoReconfig
                }
                sys::MPV_EVENT_GET_PROPERTY_REPLY if event.reply_userdata == REPLY_ROTATION => {
                    let property = &*(event.data as *const sys::mpv_event_property);
                    let rotate = (event.error >= 0 && property.format == sys::MPV_FORMAT_INT64 && !property.data.is_null()).then(|| *(property.data as *const i64));
                    self.rotation_replied(rotate);
                    Event::Other
                }
                sys::MPV_EVENT_GET_PROPERTY_REPLY if event.reply_userdata == REPLY_CROP => {
                    let property = &*(event.data as *const sys::mpv_event_property);
                    let found = (event.error >= 0 && property.format == sys::MPV_FORMAT_NODE && !property.data.is_null())
                        .then(|| read_crop(&*(property.data as *const sys::mpv_node)))
                        .flatten();
                    Event::Crop(found)
                }
                sys::MPV_EVENT_END_FILE => {
                    let end = &*(event.data as *const sys::mpv_event_end_file);
                    let error = (end.reason == sys::MPV_END_FILE_REASON_ERROR).then(|| error_text(end.error));
                    Event::EndFile { error }
                }
                sys::MPV_EVENT_PROPERTY_CHANGE => {
                    let property = &*(event.data as *const sys::mpv_event_property);
                    let id = event.reply_userdata;
                    if id == OBSERVE_ROTATION {
                        if property.format == sys::MPV_FORMAT_INT64 && !property.data.is_null() {
                            self.rotation_unconfirmed(Some(*(property.data as *const i64)));
                        }
                        return Some(Event::Other);
                    }
                    match property.format {
                        sys::MPV_FORMAT_DOUBLE => Event::Double(id, Some(*(property.data as *const f64))),
                        sys::MPV_FORMAT_FLAG => Event::Flag(id, Some(*(property.data as *const c_int) != 0)),
                        // Without a value, the property's kind is unknown; it is reported as having none.
                        _ => Event::Double(id, None),
                    }
                }
                _ => Event::Other,
            })
        }
    }

    /// Calls `callback`, from a libmpv thread, whenever events are waiting.
    pub fn set_wakeup(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let gate = self.gate.clone();
        let callback = move || {
            // Before the callback, so what it wakes finds the events marked.
            gate.events_pending.store(true, Ordering::Release);
            callback();
        };
        let data = Box::into_raw(Box::new(Box::new(callback) as Wakeup));
        // SAFETY: `data` stays alive until the player is dropped, after libmpv stops calling it.
        unsafe { sys::mpv_set_wakeup_callback(self.handle, Some(call_wakeup), data as *mut c_void) };
        if let Some(old) = self.wakeup.replace(data) {
            // SAFETY: libmpv no longer holds the old callback.
            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Calls `callback`, from a libmpv thread, whenever a new frame may be ready to render.
    pub fn set_render_update(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let data = Box::into_raw(Box::new(Box::new(callback) as Wakeup));
        // SAFETY: as for `set_wakeup`.
        unsafe { sys::mpv_render_context_set_update_callback(self.render, Some(call_wakeup), data as *mut c_void) };
        if let Some(old) = self.render_update.replace(data) {
            // SAFETY: libmpv no longer holds the old callback.
            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Whether a new frame is ready to render.
    pub fn frame_ready(&self) -> bool {
        // SAFETY: the render context is live.
        unsafe { sys::mpv_render_context_update(self.render) & sys::MPV_RENDER_UPDATE_FRAME != 0 }
    }

    /// Renders the current frame at `width` by `height` into `pixels`, as 4 bytes a pixel in BGRx order. Returns whether it did:
    /// it does not while the gate is shut, and the caller renders again once the events waiting have been read.
    pub fn render(&self, width: i32, height: i32, pixels: &mut [u8]) -> Result<bool, String> {
        let stride = width as usize * 4;
        if width <= 0 || height <= 0 || pixels.len() < stride * height as usize {
            return Err("the frame buffer is too small".to_owned());
        }
        // Rendering a rotated frame can abort the process (see `new`), and a client on the IPC socket or a changed mpv.conf can
        // still ask for rotation while the player runs.
        if !self.gate.open() {
            return Ok(false);
        }
        let mut size = [width as c_int, height as c_int];
        let format = CString::new("bgr0").expect("no nul");
        let mut stride = stride;
        let mut params = [
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_FORMAT, data: format.as_ptr() as *mut c_void },
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_POINTER, data: pixels.as_mut_ptr() as *mut c_void },
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        // SAFETY: every parameter points at data that outlives the call, and `pixels` holds the whole frame.
        check(unsafe { sys::mpv_render_context_render(self.render, params.as_mut_ptr()) })?;
        Ok(true)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: the render context goes before the handle, as libmpv requires; then the callbacks, which
        // libmpv no longer calls.
        unsafe {
            if !self.render.is_null() {
                sys::mpv_render_context_free(self.render);
            }
            sys::mpv_terminate_destroy(self.handle);
            // mpv removes its socket when it ends, unless it was stopped; the socket is not needed once it has ended.
            let _ = std::fs::remove_file(ipc_socket_path());
            for callback in [self.wakeup.take(), self.render_update.take()].into_iter().flatten() {
                drop(Box::from_raw(callback));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_split_at_each_dashed_name() {
        let options = parse_options("--no-quiet --hwdec=cuda-copy --sub-ass  --msg=a b--c --x=1 --empty=");
        let pairs: Vec<(&str, &str)> = options.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        assert_eq!(pairs, [("quiet", "no"), ("hwdec", "cuda-copy"), ("sub-ass", "yes"), ("msg", "a b--c"), ("x", "1"), ("empty", "")]);
    }

    #[test]
    fn the_front_filters_are_one_list_in_order() {
        assert_eq!(front_filters(0, false, None), None);
        let crop = CropRect { width: 404, height: 720, left: 438, top: 0 };
        assert_eq!(front_filters(0, false, Some(crop)).as_deref(), Some("@rsimgv-crop:crop=w=404:h=720:x=438:y=0"));
        let all = front_filters(90, true, Some(crop)).unwrap();
        let (upright, detector, crop) = (all.find("@rsimgv-upright:"), all.find("@rsimgv-cropdetect:"), all.find("@rsimgv-crop:"));
        assert!(upright < detector && detector < crop && upright.is_some());
        assert_eq!(all.matches(",@rsimgv-").count(), 2, "the filters are separated by commas, and the one in brackets keeps its own");
    }

    #[test]
    fn upright_filter_undoes_the_rotation() {
        assert_eq!(upright_filter(0), None);
        assert_eq!(upright_filter(360), None);
        assert_eq!(upright_filter(90).as_deref(), Some("@rsimgv-upright:lavfi=[transpose=clock]"));
        assert_eq!(upright_filter(180).as_deref(), Some("@rsimgv-upright:lavfi=[hflip,vflip]"));
        assert_eq!(upright_filter(270).as_deref(), Some("@rsimgv-upright:lavfi=[transpose=cclock]"));
        assert_eq!(upright_filter(-90), upright_filter(270));
        assert!(upright_filter(45).is_some_and(|filter| filter.contains("rotate=0.785398")));
    }
}
