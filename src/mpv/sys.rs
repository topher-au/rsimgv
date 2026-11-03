//! The parts of libmpv's C API (client.h and render.h) the player uses.

#![allow(non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_void};

#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct mpv_render_context {
    _private: [u8; 0],
}

pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_FORMAT_NODE: c_int = 6;
pub const MPV_FORMAT_NODE_MAP: c_int = 8;
pub const MPV_FORMAT_BYTE_ARRAY: c_int = 9;

pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_GET_PROPERTY_REPLY: c_int = 6;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
pub const MPV_EVENT_SEEK: c_int = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
pub const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
pub const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
pub const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
}

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

/// `LC_NUMERIC` in glibc's `locale.h`.
pub const LC_NUMERIC: c_int = 1;

unsafe extern "C" {
    pub fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

/// A value in a node: which field of the union holds it is given by `format`.
#[repr(C)]
pub union mpv_node_value {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: f64,
    pub list: *mut mpv_node_list,
    pub ba: *mut mpv_byte_array,
}

#[repr(C)]
pub struct mpv_node {
    pub u: mpv_node_value,
    pub format: c_int,
}

#[repr(C)]
pub struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub struct mpv_byte_array {
    pub data: *mut c_void,
    pub size: usize,
}

pub type wakeup_fn = unsafe extern "C" fn(*mut c_void);

#[link(name = "mpv")]
unsafe extern "C" {
    pub fn mpv_create() -> *mut mpv_handle;
    pub fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;
    pub fn mpv_terminate_destroy(ctx: *mut mpv_handle);
    pub fn mpv_error_string(error: c_int) -> *const c_char;
    pub fn mpv_set_option_string(ctx: *mut mpv_handle, name: *const c_char, data: *const c_char) -> c_int;
    pub fn mpv_command(ctx: *mut mpv_handle, args: *mut *const c_char) -> c_int;
    pub fn mpv_command_ret(ctx: *mut mpv_handle, args: *mut *const c_char, result: *mut mpv_node) -> c_int;
    pub fn mpv_free_node_contents(node: *mut mpv_node);
    pub fn mpv_command_async(ctx: *mut mpv_handle, reply_userdata: u64, args: *mut *const c_char) -> c_int;
    pub fn mpv_set_property(ctx: *mut mpv_handle, name: *const c_char, format: c_int, data: *mut c_void) -> c_int;
    pub fn mpv_set_property_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    pub fn mpv_get_property(ctx: *mut mpv_handle, name: *const c_char, format: c_int, data: *mut c_void) -> c_int;
    pub fn mpv_get_property_async(ctx: *mut mpv_handle, reply_userdata: u64, name: *const c_char, format: c_int) -> c_int;
    pub fn mpv_observe_property(ctx: *mut mpv_handle, reply_userdata: u64, name: *const c_char, format: c_int) -> c_int;
    pub fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut mpv_event;
    pub fn mpv_set_wakeup_callback(ctx: *mut mpv_handle, cb: Option<wakeup_fn>, d: *mut c_void);

    pub fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: Option<wakeup_fn>,
        callback_ctx: *mut c_void,
    );
    pub fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;
    pub fn mpv_render_context_render(ctx: *mut mpv_render_context, params: *mut mpv_render_param) -> c_int;
    pub fn mpv_render_context_free(ctx: *mut mpv_render_context);
}
