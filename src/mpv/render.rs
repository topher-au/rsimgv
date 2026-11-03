//! Renders libmpv's frames on a thread of its own, so scaling and converting them, which the software renderer does on the CPU,
//! does not hold up the main thread. The thread calls only `mpv_render_*` functions, one at a time, and hands each frame back to
//! the main thread, which makes the texture from it. Everything else in libmpv's API stays on the main thread.

use std::ffi::{CStr, CString};
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use super::gl::{GlFrame, GlRenderer, SendContext};
use super::{check, sys};

/// The most buffers kept to render frames into.
const POOL_SIZE: usize = 4;

/// The most frames handed to the main thread that it has not yet made textures from, so the thread does not run ahead of it.
const MAX_IN_FLIGHT: usize = 2;

type Pool = Arc<Mutex<Vec<Vec<u8>>>>;

/// Whether frames may be rendered, kept by the player and read by the render thread. A frame is rendered only when no event
/// from libmpv is waiting, so that a reconfiguration that came before it has been seen, and when the output is known not to be
/// rotated: libmpv's software renderer aborts the whole process on a rotated frame (see `Player::new`).
#[derive(Default)]
pub(super) struct RenderGate {
    /// Set by libmpv's wakeup, which comes with every event, and cleared once the events waiting have all been read.
    pub(super) events_pending: AtomicBool,
    /// Set when the video is reconfigured or its rotation changes, until a check made since then finds no rotation.
    pub(super) rotation_unconfirmed: AtomicBool,
}

impl RenderGate {
    /// Whether a frame may be rendered now.
    fn open(&self) -> bool {
        !self.events_pending.load(Ordering::Acquire) && !self.rotation_unconfirmed.load(Ordering::Acquire)
    }
}

/// The pixels of a frame, which go back to the pool they came from once nothing refers to them, such as when GTK is done with the
/// texture made from them. So a frame is rendered into a buffer that already exists, and handed to GTK as it is, not copied.
pub struct Pooled {
    pixels: Vec<u8>,
    pool: Pool,
}

impl Pooled {
    /// A buffer of `len` bytes from `pool`, or a new one. Its contents are whatever it held, as a render fills all of it.
    fn take(pool: &Pool, len: usize) -> Self {
        let mut buffers = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let at = buffers.iter().position(|buffer| buffer.len() == len);
        let mut pixels = match at {
            Some(at) => buffers.swap_remove(at),
            None => buffers.pop().unwrap_or_default(),
        };
        drop(buffers);
        pixels.resize(len, 0);
        Self { pixels, pool: pool.clone() }
    }
}

impl AsRef<[u8]> for Pooled {
    fn as_ref(&self) -> &[u8] {
        &self.pixels
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        let pixels = std::mem::take(&mut self.pixels);
        let mut buffers = self.pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if buffers.len() < POOL_SIZE {
            buffers.push(pixels);
        }
    }
}

/// Held while the main thread has a frame it has not made a texture from; letting go of it lets the thread render the next.
pub struct Ticket(Arc<Shared>);

impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.lock().in_flight -= 1;
        self.0.wake.notify_all();
    }
}

/// What a frame was rendered into: pixels, 4 bytes each in BGRx order, or a GL texture.
pub enum FrameImage {
    Memory(Pooled),
    Gl(GlFrame),
}

/// A frame the thread rendered, and its size.
pub struct RenderedFrame {
    pub image: FrameImage,
    pub width: i32,
    pub height: i32,
    pub ticket: Ticket,
}

#[derive(Default)]
struct State {
    /// The size to render at, in pixels.
    size: (i32, i32),
    /// Whether to render the current frame again even if no new one has come, such as at a new size.
    force: bool,
    /// Whether libmpv said a new frame may be ready.
    update: bool,
    in_flight: usize,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    pool: Pool,
    gate: Arc<RenderGate>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The render context, which only the render thread uses while it runs.
struct Context(*mut sys::mpv_render_context);

// SAFETY: libmpv allows `mpv_render_*` calls from any thread, one at a time, and the thread is the only one making them.
unsafe impl Send for Context {}

/// libmpv's callback for a frame that may be ready. It may not call libmpv, so it only tells the thread.
unsafe extern "C" fn frame_may_be_ready(data: *mut c_void) {
    // SAFETY: `data` is the `Shared` leaked in `RenderThread::start`, which is kept until the render context is freed.
    let shared = unsafe { &*(data as *const Shared) };
    shared.lock().update = true;
    shared.wake.notify_all();
}

/// The thread, and what it needs to be stopped and talked to.
pub struct RenderThread {
    shared: Arc<Shared>,
    /// The `Arc` handed to libmpv's update callback, to be let go once the render context is.
    callback_data: *const Shared,
    thread: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Starts rendering with `context`, calling `deliver`, on the render thread, with each frame. It renders at the size given by
    /// `render_at`, while `gate` is open. It renders into textures of `gl`, if it is given, and otherwise into memory.
    pub(super) fn start(context: *mut sys::mpv_render_context, gate: Arc<RenderGate>, gl: Option<SendContext>, deliver: impl Fn(RenderedFrame) + Send + 'static) -> Self {
        let shared = Arc::new(Shared { state: Mutex::default(), wake: Condvar::new(), pool: Pool::default(), gate });
        let callback_data = Arc::into_raw(shared.clone());
        // SAFETY: the context is live, and no thread is rendering with it yet.
        unsafe { sys::mpv_render_context_set_update_callback(context, Some(frame_may_be_ready), callback_data as *mut c_void) };
        let (thread_shared, context) = (shared.clone(), Context(context));
        let thread = std::thread::Builder::new().name("rsimgv-render".to_owned()).spawn(move || run(thread_shared, context, gl, deliver));
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(err) => {
                eprintln!("rsimgv: mpv: could not start the render thread: {err}");
                None
            }
        };
        Self { shared, callback_data, thread }
    }

    /// Renders the current frame at `width` by `height` now, and the frames after it at that size.
    pub fn render_at(&self, width: i32, height: i32) {
        let mut state = self.shared.lock();
        state.size = (width, height);
        state.force = true;
        drop(state);
        self.shared.wake.notify_all();
    }

    /// Lets the thread look at the gate again, after the events waiting have been read.
    pub fn wake(&self) {
        // The lock, so the thread is either waiting, and hears this, or has yet to look at the gate, and sees it open.
        drop(self.shared.lock());
        self.shared.wake.notify_all();
    }

    /// Stops the thread and waits for it, so the render context can be freed.
    pub(super) fn stop(&mut self) {
        self.shared.lock().quit = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        // SAFETY: the render context was freed before this, so libmpv no longer calls back with it, and this is the `Arc` leaked
        // in `start`.
        drop(unsafe { Arc::from_raw(self.callback_data) });
    }
}

fn run(shared: Arc<Shared>, context: Context, gl: Option<SendContext>, deliver: impl Fn(RenderedFrame)) {
    let format = CString::new("bgr0").expect("no nul");
    let mut gl = match gl.map(GlRenderer::new).transpose() {
        Ok(gl) => gl,
        Err(err) => {
            eprintln!("rsimgv: mpv: could not render with OpenGL: {err}");
            return;
        }
    };
    let mut state = shared.lock();
    loop {
        if state.quit {
            return;
        }
        let (width, height) = state.size;
        let wanted = (state.update || state.force) && width > 0 && height > 0 && state.in_flight < MAX_IN_FLIGHT && shared.gate.open();
        if !wanted {
            state = shared.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
            continue;
        }
        let force = std::mem::take(&mut state.force);
        state.update = false;
        drop(state);

        // SAFETY: the render context is live until this thread has been stopped, and only this thread calls into it.
        let new_frame = unsafe { sys::mpv_render_context_update(context.0) } & sys::MPV_RENDER_UPDATE_FRAME != 0;
        if new_frame || force {
            // Looked at again just before rendering, which takes the new frame: the gate may have shut while the frame was
            // taken. The frame stays queued in libmpv, and is rendered once the gate opens.
            if shared.gate.open() {
                let image = match &mut gl {
                    Some(gl) => gl.render(context.0, width, height).map(FrameImage::Gl),
                    None => render(&context, &format, &shared.pool, width, height).map(FrameImage::Memory),
                };
                match image {
                    Ok(image) => {
                        shared.lock().in_flight += 1;
                        deliver(RenderedFrame { image, width, height, ticket: Ticket(shared.clone()) });
                    }
                    Err(err) => eprintln!("rsimgv: mpv: could not render a frame: {err}"),
                }
            } else {
                shared.lock().force = true;
            }
        }
        state = shared.lock();
    }
}

/// Renders the current frame at `width` by `height`.
fn render(context: &Context, format: &CStr, pool: &Pool, width: i32, height: i32) -> Result<Pooled, String> {
    let mut frame = Pooled::take(pool, width as usize * height as usize * 4);
    let mut size = [width as c_int, height as c_int];
    let mut stride = width as usize * 4;
    let mut params = [
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_FORMAT, data: format.as_ptr() as *mut c_void },
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_SW_POINTER, data: frame.pixels.as_mut_ptr() as *mut c_void },
        sys::mpv_render_param { type_: sys::MPV_RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
    ];
    // SAFETY: every parameter points at data that outlives the call, and the buffer holds the whole frame.
    check(unsafe { sys::mpv_render_context_render(context.0, params.as_mut_ptr()) })?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_goes_back_to_the_pool_and_is_used_again() {
        let pool = Pool::default();
        let buffer = Pooled::take(&pool, 16);
        assert_eq!(buffer.as_ref().len(), 16);
        let address = buffer.as_ref().as_ptr();
        drop(buffer);
        assert_eq!(pool.lock().unwrap().len(), 1);
        let again = Pooled::take(&pool, 16);
        assert_eq!(again.as_ref().as_ptr(), address, "the same allocation is reused for the same size");
        assert!(pool.lock().unwrap().is_empty());
    }

    #[test]
    fn the_pool_keeps_a_few_buffers_and_resizes_one_for_another_size() {
        let pool = Pool::default();
        let buffers: Vec<Pooled> = (0..POOL_SIZE + 2).map(|_| Pooled::take(&pool, 8)).collect();
        drop(buffers);
        assert_eq!(pool.lock().unwrap().len(), POOL_SIZE);
        assert_eq!(Pooled::take(&pool, 32).as_ref().len(), 32);
    }

    #[test]
    fn letting_go_of_a_ticket_frees_a_place_for_the_next_frame() {
        let shared = Arc::new(Shared { state: Mutex::default(), wake: Condvar::new(), pool: Pool::default(), gate: Arc::default() });
        shared.lock().in_flight = 1;
        drop(Ticket(shared.clone()));
        assert_eq!(shared.lock().in_flight, 0);
    }

    #[test]
    fn the_gate_is_shut_while_events_wait_or_the_rotation_is_unconfirmed() {
        let gate = RenderGate::default();
        assert!(gate.open());
        gate.events_pending.store(true, Ordering::Release);
        assert!(!gate.open());
        gate.events_pending.store(false, Ordering::Release);
        gate.rotation_unconfirmed.store(true, Ordering::Release);
        assert!(!gate.open());
    }
}
