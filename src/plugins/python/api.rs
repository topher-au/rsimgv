//! The `rsimgv` module Python plugins import: host calls, notifications, UI helpers and reloads.

use std::cell::RefCell;
use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
#[derive(Clone)]
struct Context {
    plugin: String,
    generation: u64,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// Sets the plugin the following Python calls belong to, and restores the previous context afterwards.
pub fn with_context<T>(plugin: &str, generation: u64, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let previous = CONTEXT.with(|current| current.replace(Some(Context { plugin: plugin.to_owned(), generation })));
    let result = f();
    CONTEXT.with(|current| *current.borrow_mut() = previous);
    result
}

fn current() -> Option<Context> {
    CONTEXT.with(|current| current.borrow().clone())
}

fn current_id() -> PyResult<String> {
    current().map(|context| context.plugin).ok_or_else(|| {
        PyRuntimeError::new_err("plugin context is missing; call this from a plugin callback")
    })
}

/// Creates and registers the `rsimgv` module in `sys.modules`. Idempotent.
pub fn install(py: Python<'_>) -> PyResult<()> {
    let modules = py.import("sys")?.getattr("modules")?.cast::<PyDict>().map_err(PyErr::from)?.clone();
    if modules.contains("rsimgv")? {
        return Ok(());
    }

    let module = PyModule::new(py, "rsimgv")?;
    module.add_class::<Host>()?;
    module.add_class::<Ui>()?;
    module.add("host", Py::new(py, Host)?)?;
    module.add("ui", Py::new(py, Ui)?)?;
    module.add_function(wrap_pyfunction!(notify, &module)?)?;
    module.add_function(wrap_pyfunction!(request_reload, &module)?)?;
    module.add_function(wrap_pyfunction!(action_done, &module)?)?;
    modules.set_item("rsimgv", &module)?;
    Ok(())
}

pub fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    let json = py.import("json")?;
    Ok(json.call_method1("loads", (value.to_string(),))?.unbind())
}

pub fn py_to_value(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    let json = object.py().import("json")?;
    let text: String = json.call_method1("dumps", (object,))?.extract()?;
    serde_json::from_str(&text).map_err(|err| PyValueError::new_err(err.to_string()))
}

/// The host object exposed as `rsimgv.host`.
#[pyclass]
pub struct Host;

#[pymethods]
impl Host {
    #[getter]
    fn plugin_id(&self) -> Option<String> {
        current().map(|context| context.plugin)
    }

    #[getter]
    fn generation(&self) -> u64 {
        current().map_or(0, |context| context.generation)
    }

    #[pyo3(signature = (method, params=None))]
    fn call(&self, py: Python<'_>, method: String, params: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        if !gtk::glib::MainContext::default().is_owner() {
            return Err(PyRuntimeError::new_err("host.call must run on the GTK main thread"));
        }
        let params = match params {
            Some(params) => py_to_value(params.bind(py))?,
            None => Value::Object(serde_json::Map::new()),
        };
        let result = crate::plugins::host_call(&method, &params).map_err(PyRuntimeError::new_err)?;
        value_to_py(py, &result)
    }

    fn log(&self, level: String, message: String) {
        eprintln!("rsimgv: plugin {level}: {message}");
    }

    fn config_path(&self) -> PyResult<String> {
        Ok(crate::plugins::config_path(&current_id()?))
    }
}

/// The UI object exposed as `rsimgv.ui`.
#[pyclass]
pub struct Ui;

fn main_thread() -> PyResult<()> {
    if gtk::glib::MainContext::default().is_owner() {
        Ok(())
    } else {
        Err(PyRuntimeError::new_err("this rsimgv.ui call must run on the GTK main thread"))
    }
}

#[pymethods]
impl Ui {
    /// Connects a GTK signal and records it so a reload disconnects it.
    fn connect(&self, py: Python<'_>, widget: Py<PyAny>, signal: String, callback: Py<PyAny>) -> PyResult<u64> {
        main_thread()?;
        let plugin = current_id()?;
        let id: u64 = widget.bind(py).call_method1("connect", (signal, callback))?.extract()?;
        crate::plugins::track_signal(&plugin, widget, id);
        Ok(id)
    }

    /// Queues a callable for the GTK main thread. Safe from a plugin worker thread.
    fn idle_add(&self, callback: Py<PyAny>) -> PyResult<u64> {
        crate::plugins::queue_idle(callback);
        Ok(0)
    }

    /// Runs a callable every `ms` milliseconds until it returns false.
    fn timeout_add(&self, ms: u64, callback: Py<PyAny>) -> PyResult<u32> {
        main_thread()?;
        let plugin = current_id()?;
        let source = gtk::glib::timeout_add_local(Duration::from_millis(ms.max(1)), move || {
            let again = Python::attach(|py| match callback.bind(py).call0() {
                Ok(value) => value.is_truthy().unwrap_or(false),
                Err(err) => {
                    err.print(py);
                    false
                }
            });
            if again {
                gtk::glib::ControlFlow::Continue
            } else {
                gtk::glib::ControlFlow::Break
            }
        });
        let raw = source.as_raw();
        crate::plugins::track_source(&plugin, raw);
        Ok(raw)
    }

    fn source_remove(&self, id: u32) {
        unsafe { gtk::glib::ffi::g_source_remove(id) };
    }

    fn disconnect_all(&self) -> PyResult<()> {
        main_thread()?;
        crate::plugins::disconnect_signals(&current_id()?);
        Ok(())
    }
}

/// Shows a message over the window.
#[pyfunction]
#[pyo3(signature = (text, level="info", seconds=4.0))]
fn notify(text: String, level: &str, seconds: f64) -> PyResult<()> {
    let params = json!({"text": text, "level": level, "seconds": seconds});
    crate::plugins::host_call("messages.show", &params).map_err(PyRuntimeError::new_err)?;
    Ok(())
}

/// Asks the host to reload the calling plugin after the current callback returns.
#[pyfunction]
fn request_reload() -> PyResult<()> {
    let id = current_id()?;
    crate::plugins::request_reload(&id);
    Ok(())
}

/// Reports the result of an action. An IPC `actions.run` waits for this and returns it to the caller.
/// A dictionary with an `error` key is returned as an error; any other value is returned as the result.
#[pyfunction]
fn action_done(py: Python<'_>, result: Option<Py<PyAny>>) -> PyResult<()> {
    let value = match result {
        Some(result) => py_to_value(result.bind(py))?,
        None => Value::Null,
    };
    crate::plugins::action_done(value);
    Ok(())
}
