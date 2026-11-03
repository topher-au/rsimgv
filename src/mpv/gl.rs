//! Renders libmpv's frames with OpenGL, into textures GTK draws without copying them. The context has no surface of its own and
//! shares its textures with GTK's. It is made current on the main thread to make the render context, then on the render thread.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use gtk::gdk;
use gtk::gdk::prelude::*;
use gtk::glib::translate::ToGlibPtr;

use super::{check, sys};

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: c_int = 0x2601;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;
const GL_RGBA: u32 = 0x1908;
const GL_RGBA8: c_int = 0x8058;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_SYNC_GPU_COMMANDS_COMPLETE: u32 = 0x9117;

const RTLD_NOW: c_int = 2;

unsafe extern "C" {
    fn dlopen(file: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(library: *mut c_void, name: *const c_char) -> *mut c_void;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut c_void;
}

type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// GDK uses EGL or GLX, so the loader is that of whichever has a current context.
fn find_loader() -> Option<GetProcAddress> {
    for (library, current, loader) in [(c"libEGL.so.1", c"eglGetCurrentContext", c"eglGetProcAddress"), (c"libGL.so.1", c"glXGetCurrentContext", c"glXGetProcAddressARB")] {
        // SAFETY: the names are valid strings, and these symbols have these signatures in those libraries.
        unsafe {
            let library = dlopen(library.as_ptr(), RTLD_NOW);
            if library.is_null() {
                continue;
            }
            let (current, loader) = (dlsym(library, current.as_ptr()), dlsym(library, loader.as_ptr()));
            if current.is_null() || loader.is_null() {
                continue;
            }
            let current: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(current);
            if !current().is_null() {
                return Some(std::mem::transmute::<*mut c_void, GetProcAddress>(loader));
            }
        }
    }
    None
}

/// The address of the GL function `name`, in the current context. libmpv calls this with it current too.
unsafe extern "C" fn get_proc_address(_data: *mut c_void, name: *const c_char) -> *mut c_void {
    static LOADER: OnceLock<Option<GetProcAddress>> = OnceLock::new();
    match LOADER.get_or_init(find_loader) {
        // SAFETY: `name` is the valid string libmpv or `Functions::load` gave.
        Some(loader) => unsafe { loader(name) },
        None => std::ptr::null_mut(),
    }
}

/// A new context to render with, made while the display has no need of a surface for it.
pub(super) fn new_context() -> Result<gdk::GLContext, String> {
    let display = gdk::Display::default().ok_or("no display")?;
    let context = display.create_gl_context().map_err(|err| err.to_string())?;
    context.realize().map_err(|err| err.to_string())?;
    Ok(context)
}

/// Makes the render context of `handle` with `context`, which leaves no context current on this thread.
pub(super) fn create_render_context(render: &mut *mut sys::mpv_render_context, handle: *mut sys::mpv_handle, context: &gdk::GLContext) -> Result<(), String> {
    let api = CString::new("opengl").expect("no nul");
    let mut init = sys::mpv_opengl_init_params { get_proc_address: Some(get_proc_address), get_proc_address_ctx: std::ptr::null_mut() };
    let mut params = vec![
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
    ];
    // Hardware decoders on Wayland need the display to share their surfaces.
    if let Some(display) = gdk::Display::default().filter(|display| display.type_().name() == "GdkWaylandDisplay") {
        let raw: *mut gdk::ffi::GdkDisplay = display.to_glib_none().0;
        // SAFETY: this is a Wayland display, as checked.
        let wl_display = unsafe { gdk_wayland_display_get_wl_display(raw as *mut c_void) };
        params.push(sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_WL_DISPLAY, data: wl_display });
    }
    params.push(sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() });
    context.make_current();
    // SAFETY: the context is current, and the parameters outlive the call.
    let result = check(unsafe { sys::mpv_render_context_create(render, handle, params.as_mut_ptr()) });
    gdk::GLContext::clear_current();
    result
}

/// Frees the render context, which needs `context` current as it holds GL objects. Nothing may be rendering with either.
pub(super) fn free_render_context(render: *mut sys::mpv_render_context, context: &gdk::GLContext) {
    context.make_current();
    // SAFETY: as above.
    unsafe { sys::mpv_render_context_free(render) };
    gdk::GLContext::clear_current();
}

/// Declares the GL functions used and loads them by name.
macro_rules! gl_functions {
    ($($field:ident = $name:literal: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        struct Functions {
            $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl Functions {
            /// The functions of the current context, or the name of one it does not have.
            fn load() -> Result<Self, String> {
                // SAFETY: the name is a valid string, and a function of that name has the signature declared.
                unsafe {
                    Ok(Self {
                        $($field: {
                            let address = get_proc_address(std::ptr::null_mut(), concat!($name, "\0").as_ptr() as *const c_char);
                            if address.is_null() {
                                return Err(format!("no {}", $name));
                            }
                            std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(address)
                        },)*
                    })
                }
            }
        }
    };
}

gl_functions! {
    gen_textures = "glGenTextures": fn(c_int, *mut u32);
    delete_textures = "glDeleteTextures": fn(c_int, *const u32);
    bind_texture = "glBindTexture": fn(u32, u32);
    tex_parameteri = "glTexParameteri": fn(u32, u32, c_int);
    tex_image_2d = "glTexImage2D": fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void);
    gen_framebuffers = "glGenFramebuffers": fn(c_int, *mut u32);
    delete_framebuffers = "glDeleteFramebuffers": fn(c_int, *const u32);
    bind_framebuffer = "glBindFramebuffer": fn(u32, u32);
    framebuffer_texture_2d = "glFramebufferTexture2D": fn(u32, u32, u32, u32, c_int);
    check_framebuffer_status = "glCheckFramebufferStatus": fn(u32) -> u32;
    fence_sync = "glFenceSync": fn(u32, u32) -> *mut c_void;
    delete_sync = "glDeleteSync": fn(*mut c_void);
    flush = "glFlush": fn();
}

/// A texture to render into, with its size.
#[derive(Clone, Copy)]
struct Texture {
    id: u32,
    width: i32,
    height: i32,
}

/// A texture GTK is done with, and the fence it was drawn after, which only the render thread may delete.
struct Finished {
    texture: Texture,
    sync: usize,
}

/// A frame rendered into a texture, which goes back to the render thread once GTK is done with what `into_texture` makes.
pub struct GlFrame {
    texture: Texture,
    sync: *mut c_void,
    context: gdk::GLContext,
    /// Where it goes back to if it is dropped, such as when its video has stopped; `into_texture` takes it for GTK's release.
    finished: Option<Arc<Mutex<Vec<Finished>>>>,
}

// SAFETY: the frame only passes from the render thread to the main thread, and nothing on the render thread keeps the context's
// object or the fence in use while it is on its way.
unsafe impl Send for GlFrame {}

impl GlFrame {
    /// A GDK texture of the frame.
    pub fn into_texture(mut self) -> gdk::Texture {
        let builder = gdk::GLTextureBuilder::new().set_context(Some(&self.context)).set_id(self.texture.id).set_width(self.texture.width).set_height(self.texture.height).set_sync(Some(self.sync));
        let (texture, sync, finished) = (self.texture, self.sync as usize, self.finished.take());
        // SAFETY: the texture holds this size of this context's texture, and is not rendered into until the function below is called.
        unsafe {
            builder.build_with_release_func(move || {
                if let Some(finished) = finished {
                    finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(Finished { texture, sync });
                }
            })
        }
    }
}

impl Drop for GlFrame {
    fn drop(&mut self) {
        if let Some(finished) = self.finished.take() {
            finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(Finished { texture: self.texture, sync: self.sync as usize });
        }
    }
}

/// The context, for the render thread.
pub(super) struct SendContext(pub(super) gdk::GLContext);

// SAFETY: the context is current on one thread at a time, as `create_render_context` and `free_render_context` leave it not current,
// and the render thread has stopped before the latter.
unsafe impl Send for SendContext {}

/// What the render thread renders with.
pub(super) struct GlRenderer {
    functions: Functions,
    context: gdk::GLContext,
    framebuffer: u32,
    free: Vec<Texture>,
    finished: Arc<Mutex<Vec<Finished>>>,
}

impl GlRenderer {
    /// Makes `context` current on this thread, until this is dropped.
    pub(super) fn new(context: SendContext) -> Result<Self, String> {
        let context = context.0;
        context.make_current();
        let functions = Functions::load()?;
        let mut framebuffer = 0;
        // SAFETY: the context is current, and the function is from it.
        unsafe { (functions.gen_framebuffers)(1, &mut framebuffer) };
        Ok(Self { functions, context, framebuffer, free: Vec::new(), finished: Arc::default() })
    }

    /// A texture of this size: one GTK is done with, or a new one. Fences of finished textures are deleted, and textures of another
    /// size, from before a resize.
    fn take_texture(&mut self, width: i32, height: i32) -> Texture {
        let f = &self.functions;
        let finished = std::mem::take(&mut *self.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        for Finished { texture, sync } in finished {
            // SAFETY: the context is current, and GTK has finished with the fence and the texture.
            unsafe { (f.delete_sync)(sync as *mut c_void) };
            self.free.push(texture);
        }
        let mut found = None;
        self.free.retain(|texture| {
            if found.is_none() && (texture.width, texture.height) == (width, height) {
                found = Some(*texture);
                return false;
            }
            if (texture.width, texture.height) != (width, height) {
                // SAFETY: as above.
                unsafe { (f.delete_textures)(1, &texture.id) };
                return false;
            }
            true
        });
        found.unwrap_or_else(|| {
            let mut id = 0;
            // SAFETY: the context is current, and no data is read for the texture.
            unsafe {
                (f.gen_textures)(1, &mut id);
                (f.bind_texture)(GL_TEXTURE_2D, id);
                (f.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
                (f.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
                (f.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
                (f.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
                (f.tex_image_2d)(GL_TEXTURE_2D, 0, GL_RGBA8, width, height, 0, GL_RGBA, GL_UNSIGNED_BYTE, std::ptr::null());
                (f.bind_texture)(GL_TEXTURE_2D, 0);
            }
            Texture { id, width, height }
        })
    }

    /// Renders the current frame at `width` by `height`.
    pub(super) fn render(&mut self, render: *mut sys::mpv_render_context, width: i32, height: i32) -> Result<GlFrame, String> {
        let texture = self.take_texture(width, height);
        let f = &self.functions;
        // SAFETY: the context is current, and the texture and framebuffer are from it.
        let status = unsafe {
            (f.bind_framebuffer)(GL_FRAMEBUFFER, self.framebuffer);
            (f.framebuffer_texture_2d)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture.id, 0);
            (f.check_framebuffer_status)(GL_FRAMEBUFFER)
        };
        if status != GL_FRAMEBUFFER_COMPLETE {
            self.free.push(texture);
            return Err(format!("the framebuffer is not complete: {status:#x}"));
        }
        let mut target = sys::mpv_opengl_fbo { fbo: self.framebuffer as c_int, w: width, h: height, internal_format: GL_RGBA8 };
        // GDK takes a texture's first row as the top of the picture, as libmpv renders it unflipped.
        let mut params = [
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
            sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        // SAFETY: the context is current, and the parameters outlive the call.
        let result = check(unsafe { sys::mpv_render_context_render(render, params.as_mut_ptr()) });
        // SAFETY: as above. GTK waits for the fence, as its context is another, and flushing here lets that wait end.
        let sync = unsafe {
            let sync = (f.fence_sync)(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
            (f.bind_framebuffer)(GL_FRAMEBUFFER, 0);
            (f.flush)();
            sync
        };
        if let Err(err) = result {
            // SAFETY: as above.
            unsafe { (f.delete_sync)(sync) };
            self.free.push(texture);
            return Err(err);
        }
        Ok(GlFrame { texture, sync, context: self.context.clone(), finished: Some(self.finished.clone()) })
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        let f = &self.functions;
        let finished = std::mem::take(&mut *self.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        // SAFETY: the context is current, and nothing uses these any more. Textures GTK still holds are left for the context's end.
        unsafe {
            for Finished { texture, sync } in finished {
                (f.delete_sync)(sync as *mut c_void);
                (f.delete_textures)(1, &texture.id);
            }
            for texture in &self.free {
                (f.delete_textures)(1, &texture.id);
            }
            (f.delete_framebuffers)(1, &self.framebuffer);
        }
        gdk::GLContext::clear_current();
    }
}
