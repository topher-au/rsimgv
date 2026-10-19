//! A small, safe wrapper over libmpv: a player that loads files, takes commands and properties, reports
//! events, and renders frames in software into memory for GTK to show. Each player listens for mpv JSON IPC on
//! `rsimgv-mpv.sock` in the runtime directory. See `stream` for the GTK side.

mod stream;
mod sys;

pub use stream::MpvStream;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

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
    Shutdown,
    Other,
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

/// Where the player's mpv IPC socket is: `rsimgv-mpv.sock` in the user's runtime directory.
pub fn ipc_socket_path() -> std::path::PathBuf {
    gtk::glib::user_runtime_dir().join("rsimgv-mpv.sock")
}

/// A libmpv player with a software render context.
pub struct Player {
    handle: *mut sys::mpv_handle,
    render: *mut sys::mpv_render_context,
    /// The callbacks handed to libmpv, freed once it is gone.
    wakeup: Option<*mut Wakeup>,
    render_update: Option<*mut Wakeup>,
}

impl Player {
    /// A player set up to play without a window of its own, rendering into memory.
    pub fn new() -> Result<Self, String> {
        // SAFETY: plain libmpv calls on a handle this function owns until it is returned.
        unsafe {
            // libmpv refuses to start unless numbers are formatted the C way, and GTK sets the locale from the
            // environment at startup. Only the number format changes; text and dates stay localised.
            sys::setlocale(sys::LC_NUMERIC, c"C".as_ptr());
            let handle = sys::mpv_create();
            if handle.is_null() {
                return Err("could not create the mpv player".to_owned());
            }
            let mut player = Self { handle, render: std::ptr::null_mut(), wakeup: None, render_update: None };
            for (name, value) in [
                ("vo", "libmpv"),
                ("hwdec", "auto-safe"),
                ("keep-open", "yes"),
                ("idle", "yes"),
                ("terminal", "no"),
                ("input-default-bindings", "no"),
                ("osc", "no"),
                ("ytdl", "no"),
            ] {
                player.set_option(name, value)?;
            }
            // Other programs can drive the player through mpv's JSON IPC on this socket.
            let socket = ipc_socket_path();
            if let Err(err) = player.set_option("input-ipc-server", &socket.to_string_lossy()) {
                eprintln!("rsimgv: mpv: could not open the IPC socket {}: {err}", socket.display());
            }
            check(sys::mpv_initialize(handle))?;

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

    /// The next waiting event, or `None` once there are none.
    pub fn next_event(&self) -> Option<Event> {
        // SAFETY: the event is valid until the next call, and is read fully before then.
        unsafe {
            let event = &*sys::mpv_wait_event(self.handle, 0.0);
            Some(match event.event_id {
                sys::MPV_EVENT_NONE => return None,
                sys::MPV_EVENT_SHUTDOWN => Event::Shutdown,
                sys::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
                sys::MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
                sys::MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
                sys::MPV_EVENT_END_FILE => {
                    let end = &*(event.data as *const sys::mpv_event_end_file);
                    let error = (end.reason == sys::MPV_END_FILE_REASON_ERROR).then(|| error_text(end.error));
                    Event::EndFile { error }
                }
                sys::MPV_EVENT_PROPERTY_CHANGE => {
                    let property = &*(event.data as *const sys::mpv_event_property);
                    let id = event.reply_userdata;
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

    /// Renders the current frame at `width` by `height` into `pixels`, as 4 bytes a pixel in BGRx order.
    pub fn render(&self, width: i32, height: i32, pixels: &mut [u8]) -> Result<(), String> {
        let stride = width as usize * 4;
        if width <= 0 || height <= 0 || pixels.len() < stride * height as usize {
            return Err("the frame buffer is too small".to_owned());
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
        check(unsafe { sys::mpv_render_context_render(self.render, params.as_mut_ptr()) })
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
            for callback in [self.wakeup.take(), self.render_update.take()].into_iter().flatten() {
                drop(Box::from_raw(callback));
            }
        }
    }
}
