//! The C bridge to PyGObject. See `native/pygobject_bridge.c`.

use std::ffi::c_int;
use std::os::raw::c_void;

use pyo3::ffi::PyObject;

#[allow(dead_code)]
unsafe extern "C" {
    fn rsimgv_pygobject_init() -> c_int;
    fn rsimgv_widget_to_py(widget: *mut c_void) -> *mut PyObject;
    fn rsimgv_py_to_widget(object: *mut PyObject) -> *mut c_void;
}

/// Calls `pygobject_init`, which loads the PyGObject API table. Must be called with the GIL held.
pub unsafe fn init() -> Result<(), String> {
    if rsimgv_pygobject_init() == 0 {
        return Err("pygobject_init failed; is python3-gobject installed?".to_owned());
    }
    Ok(())
}

/// Wraps a `GtkWidget*` in a new Python `Gtk.Widget` object. The caller owns the returned reference.
pub unsafe fn widget_to_py(widget: *mut c_void) -> *mut PyObject {
    rsimgv_widget_to_py(widget)
}

/// Returns the `GtkWidget*` behind a Python object, or null when it is not a GTK widget.
#[allow(dead_code)]
pub unsafe fn py_to_widget(object: *mut PyObject) -> *mut c_void {
    rsimgv_py_to_widget(object)
}
