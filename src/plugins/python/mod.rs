//! The Python backend: embeds CPython, imports plugin modules, and routes callbacks.

use std::path::Path;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::Value;

use gtk::glib::translate::ToGlibPtr;

use super::manifest::Manifest;
use super::bridge;

mod api;

pub struct PythonPlugin {
    pub generation: u64,
    pub module_name: String,
    pub instance: Py<PyAny>,
}

/// The module name a plugin is imported under. It is prefixed so a plugin cannot shadow a normal module and so a
/// reload can remove exactly its modules from `sys.modules`.
pub fn module_name(id: &str) -> String {
    let suffix: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("rsimgv_plugin_{suffix}")
}

pub fn init() -> Result<(), String> {
    Python::attach(|py| {
        api::install(py).map_err(|err| err.to_string())?;
        unsafe { bridge::init() }.map_err(|err| format!("PyGObject is unavailable: {err}"))
    })
}

pub fn load_plugin(manifest: &Manifest, dir: &Path, generation: u64) -> Result<PythonPlugin, String> {
    Python::attach(|py| load_inner(py, manifest, dir, generation)).map_err(|err| err.to_string())
}

fn load_inner(py: Python<'_>, manifest: &Manifest, dir: &Path, generation: u64) -> PyResult<PythonPlugin> {
    api::install(py)?;
    let module_name = module_name(&manifest.id);
    add_sys_path(py, dir)?;
    remove_modules(py, &module_name)?;

    let entry = manifest.entry_path(dir);
    let importlib = py.import("importlib.util")?;
    let spec = importlib
        .getattr("spec_from_file_location")?
        .call1((&module_name, entry.to_string_lossy().into_owned()))?;
    let module = importlib.getattr("module_from_spec")?.call1((&spec,))?;
    sys_modules(py)?.bind(py).set_item(&module_name, &module)?;
    spec.getattr("loader")?.call_method1("exec_module", (&module,))?;

    let instance: Py<PyAny> = if module.hasattr("create_plugin")? {
        module.getattr("create_plugin")?.call0()?.unbind()
    } else {
        module.getattr("Plugin")?.call0()?.unbind()
    };

    api::with_context(&manifest.id, generation, || {
        instance.bind(py).call_method1("activate", (py.None(),))?;
        Ok(())
    })?;

    Ok(PythonPlugin { generation, module_name, instance })
}

fn add_sys_path(py: Python<'_>, dir: &Path) -> PyResult<()> {
    let path = py.import("sys")?.getattr("path")?.cast::<PyList>()?.clone();
    let text = dir.to_string_lossy().into_owned();
    if !path.iter().any(|item| item.extract::<String>().is_ok_and(|item| item == text)) {
        path.insert(0, text)?;
    }
    Ok(())
}

fn sys_modules(py: Python<'_>) -> PyResult<Py<PyDict>> {
    Ok(py.import("sys")?.getattr("modules")?.cast::<PyDict>().map_err(PyErr::from)?.clone().unbind())
}

fn remove_modules(py: Python<'_>, module_name: &str) -> PyResult<()> {
    let modules = sys_modules(py)?;
    let modules = modules.bind(py);
    let keys: Vec<String> = modules
        .keys()
        .iter()
        .filter_map(|key| key.extract::<String>().ok())
        .filter(|key| key == module_name || key.starts_with(&format!("{module_name}.")))
        .collect();
    for key in keys {
        modules.del_item(key)?;
    }
    py.import("importlib")?.call_method0("invalidate_caches")?;
    Ok(())
}

/// Removes a plugin's modules after `deactivate`, so the next reload imports fresh code.
pub fn purge(module_name: &str) {
    Python::attach(|py| {
        let _ = remove_modules(py, module_name);
        let _ = py.import("gc").and_then(|gc| gc.call_method0("collect"));
    });
}

pub fn deactivate(instance: &Py<PyAny>) -> Result<(), String> {
    Python::attach(|py| {
        instance
            .bind(py)
            .call_method0("deactivate")
            .map(|_| ())
            .map_err(|err| {
                err.print(py);
                err.to_string()
            })
    })
}

pub fn invoke_action(
    instance: &Py<PyAny>,
    plugin: &str,
    generation: u64,
    action: &str,
    context: &Value,
) -> Result<(), String> {
    Python::attach(|py| {
        api::with_context(plugin, generation, || {
            let context = api::value_to_py(py, context)?;
            instance.bind(py).call_method1("invoke_action", (action, context))?;
            Ok(())
        })
        .map_err(|err: PyErr| {
            err.print(py);
            err.to_string()
        })
    })
}

pub fn on_event(
    instance: &Py<PyAny>,
    plugin: &str,
    generation: u64,
    name: &str,
    data: &Value,
) -> Result<(), String> {
    Python::attach(|py| {
        api::with_context(plugin, generation, || {
            let data = api::value_to_py(py, data)?;
            instance.bind(py).call_method1("on_event", (name, data))?;
            Ok(())
        })
        .map_err(|err: PyErr| {
            err.print(py);
            err.to_string()
        })
    })
}

pub fn create_panel(
    instance: &Py<PyAny>,
    plugin: &str,
    generation: u64,
    panel_id: &str,
    instance_id: u64,
    container: &gtk::Box,
    context: &Value,
) -> Result<(), String> {
    Python::attach(|py| {
        api::with_context(plugin, generation, || {
            let pointer =
                <gtk::Box as ToGlibPtr<*mut gtk::ffi::GtkBox>>::to_glib_none(container).0 as *mut std::ffi::c_void;
            let object = unsafe { bridge::widget_to_py(pointer) };
            if object.is_null() {
                return Err(pyo3::exceptions::PyRuntimeError::new_err("could not wrap the panel container"));
            }
            let container = unsafe { pyo3::Bound::<PyAny>::from_owned_ptr(py, object).unbind() };
            let context = api::value_to_py(py, context)?;
            instance
                .bind(py)
                .call_method1("create_panel", (panel_id, instance_id, container, context))?;
            Ok(())
        })
        .map_err(|err: PyErr| {
            err.print(py);
            err.to_string()
        })
    })
}

pub fn panel_destroyed(instance: &Py<PyAny>, panel_id: &str, instance_id: u64) -> Result<(), String> {
    Python::attach(|py| {
        instance
            .bind(py)
            .call_method1("panel_destroyed", (panel_id, instance_id))
            .map(|_| ())
            .map_err(|err| {
                err.print(py);
                err.to_string()
            })
    })
}

pub fn run_idle(callback: Py<PyAny>) {
    Python::attach(|py| {
        if let Err(err) = callback.bind(py).call0() {
            err.print(py);
        }
    });
}
