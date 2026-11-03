//! KDE Plasma's blur for the parts of the window that float over the item: KWin blurs behind the window's surface
//! within the regions the window names, through the `org_kde_kwin_blur` protocol (the C side is in `native/kde_blur.c`).
//! It is only available on Wayland under KWin; elsewhere nothing is sent.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::raw::c_int;

use gtk::prelude::*;

extern "C" {
    fn rsimgv_kde_blur_connect(display: *mut c_void);
    fn rsimgv_kde_blur_set(surface: *mut c_void, count: c_int, rects: *const c_int) -> c_int;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut c_void;
    fn gdk_wayland_surface_get_wl_surface(surface: *mut c_void) -> *mut c_void;
}

/// The window's blur regions, as `x, y, width, height` for each widget shown, in the window's surface.
fn regions(window: &gtk::Window, widgets: &[gtk::Widget]) -> Vec<c_int> {
    widgets
        .iter()
        .filter(|widget| widget.is_mapped())
        .filter_map(|widget| widget.compute_bounds(window))
        .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
        .flat_map(|bounds| {
            [
                bounds.x() as c_int,
                bounds.y() as c_int,
                bounds.width().ceil() as c_int,
                bounds.height().ceil() as c_int,
            ]
        })
        .collect()
}

/// Makes KWin blur behind `widgets` in `window`, if they moved or changed since `last`. No widgets stops the blur.
/// Does nothing unless the window is on Wayland.
pub fn update(window: &gtk::Window, widgets: &[gtk::Widget], last: &RefCell<Vec<c_int>>) {
    if gtk::prelude::WidgetExt::display(window).type_().name() != "GdkWaylandDisplay" {
        return;
    }
    let Some(surface) = window.surface() else { return };
    let rects = regions(window, widgets);
    if *last.borrow() == rects {
        return;
    }
    let sent = unsafe {
        let display = gtk::prelude::WidgetExt::display(window);
        let wl_display = gdk_wayland_display_get_wl_display(display.as_ptr() as *mut c_void);
        rsimgv_kde_blur_connect(wl_display);
        let wl_surface = gdk_wayland_surface_get_wl_surface(surface.as_ptr() as *mut c_void);
        rsimgv_kde_blur_set(wl_surface, (rects.len() / 4) as c_int, rects.as_ptr())
    };
    // Until the compositor's blur is available the change is kept back, so it is sent on a later frame.
    if sent == 0 {
        return;
    }
    last.replace(rects);
    // The blur takes effect with the window's next commit, so the window is drawn again to make it.
    window.queue_draw();
}
