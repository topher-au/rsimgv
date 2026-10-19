//! YAML storage for the `yaml` GSettings backend (see backend.c).
//!
//! The file is `~/.config/rsimgv/rsimgv.conf` unless `RSIMGV_CONFIG` names another path. Settings live
//! under one `settings` mapping, keyed by the schema key's name:
//!
//! ```yaml
//! settings:
//!   item-size: 128
//!   media-only: true
//! ```
//!
//! Values are read back as the type GSettings asks for. Changes made in the file while the app runs
//! are picked up by a file monitor, which tells GSettings about every key that changed.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use glib::prelude::ToVariant;
use glib::translate::{FromGlibPtrNone, IntoGlibPtr};
use gtk::{gio, glib};
use serde_yaml_ng::{Mapping, Number, Value};

/// The schema path that GSettings prefixes to every key the backend is asked for.
const PATH_PREFIX: &str = "/au/topher/rimgv/";

struct Store {
    backend: *mut gio::ffi::GSettingsBackend,
    file: PathBuf,
    values: BTreeMap<String, Value>,
    /// Kept alive for as long as the store is, so the file is watched.
    _monitor: Option<gio::FileMonitor>,
}

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
}

/// Where the settings file is: `RSIMGV_CONFIG` if set, otherwise `~/.config/rsimgv/rsimgv.conf`.
pub fn config_path() -> PathBuf {
    std::env::var_os("RSIMGV_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::user_config_dir().join("rsimgv").join("rsimgv.conf"))
}

/// The schema key name for a GSettings key path.
fn short_key(key: &str) -> &str {
    key.strip_prefix(PATH_PREFIX).unwrap_or(key)
}

/// Reads the settings mapping from `file`. A missing or empty file has no settings.
fn load(file: &Path) -> BTreeMap<String, Value> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    let root: Value = serde_yaml_ng::from_str(&text).unwrap_or(Value::Null);
    root.get("settings")
        .and_then(Value::as_mapping)
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// Writes the settings to `file` through a temporary file, so a crash never leaves it half-written.
fn save(file: &Path, values: &BTreeMap<String, Value>) -> std::io::Result<()> {
    let mut settings = Mapping::new();
    for (key, value) in values {
        settings.insert(Value::String(key.clone()), value.clone());
    }
    let mut root = Mapping::new();
    root.insert(Value::String("settings".into()), Value::Mapping(settings));
    let text = serde_yaml_ng::to_string(&Value::Mapping(root)).map_err(std::io::Error::other)?;

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = file.with_extension("conf.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(temp, file)
}

/// A YAML value as the GVariant of type `ty`, if it fits that type.
fn from_yaml(value: &Value, ty: &glib::VariantTy) -> Option<glib::Variant> {
    match ty.as_str() {
        "b" => value.as_bool().map(|b| b.to_variant()),
        "i" => value.as_i64().and_then(|n| i32::try_from(n).ok()).map(|n| n.to_variant()),
        "u" => value.as_u64().and_then(|n| u32::try_from(n).ok()).map(|n| n.to_variant()),
        "x" => value.as_i64().map(|n| n.to_variant()),
        "t" => value.as_u64().map(|n| n.to_variant()),
        "d" => value.as_f64().map(|n| n.to_variant()),
        "s" => value.as_str().map(|s| s.to_variant()),
        // Anything else is stored as GVariant text.
        _ => value
            .as_str()
            .and_then(|text| glib::Variant::parse(Some(ty), text).ok()),
    }
}

/// A GVariant as the YAML value that stores it.
fn to_yaml(value: &glib::Variant) -> Value {
    match value.type_().as_str() {
        "b" => value.get::<bool>().map(Value::Bool),
        "i" => value.get::<i32>().map(|n| Value::Number(Number::from(i64::from(n)))),
        "u" => value.get::<u32>().map(|n| Value::Number(Number::from(u64::from(n)))),
        "x" => value.get::<i64>().map(|n| Value::Number(Number::from(n))),
        "t" => value.get::<u64>().map(|n| Value::Number(Number::from(n))),
        "d" => value.get::<f64>().map(|n| Value::Number(Number::from(n))),
        "s" => value.get::<String>().map(Value::String),
        _ => None,
    }
    .unwrap_or_else(|| Value::String(value.print(true).to_string()))
}

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> Option<R> {
    STORE.with(|store| store.borrow_mut().as_mut().map(f))
}

/// Tells GSettings that `key` (a short key name) changed.
fn notify_changed(backend: *mut gio::ffi::GSettingsBackend, key: &str) {
    let full = format!("{PATH_PREFIX}{key}");
    let Ok(full) = CString::new(full) else { return };
    unsafe {
        gio::ffi::g_settings_backend_changed(backend, full.as_ptr(), std::ptr::null_mut());
    }
}

/// Re-reads the file and reports every key whose value changed.
fn reload(file: &Path) {
    let fresh = load(file);
    let changed: Vec<(String, *mut gio::ffi::GSettingsBackend)> = with_store(|store| {
        let keys: Vec<String> = store.values.keys().chain(fresh.keys()).cloned().collect();
        let mut changed = Vec::new();
        for key in keys {
            if store.values.get(&key) != fresh.get(&key) {
                changed.push((key, store.backend));
            }
        }
        store.values = fresh;
        changed
    })
    .unwrap_or_default();
    for (key, backend) in changed {
        notify_changed(backend, &key);
    }
}

/// Starts the store for `backend`: loads the file and watches it.
#[no_mangle]
pub extern "C" fn rsimgv_store_init(backend: *mut gio::ffi::GSettingsBackend) {
    let file = config_path();
    let values = load(&file);

    let monitor = gio::File::for_path(&file)
        .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        .ok();
    if let Some(monitor) = &monitor {
        let watched = file.clone();
        monitor.connect_changed(move |_, _, _, event| {
            if matches!(
                event,
                gio::FileMonitorEvent::Changed
                    | gio::FileMonitorEvent::Created
                    | gio::FileMonitorEvent::Deleted
                    | gio::FileMonitorEvent::MovedIn
            ) {
                reload(&watched);
            }
        });
    }

    STORE.with(|store| {
        *store.borrow_mut() = Some(Store {
            backend,
            file,
            values,
            _monitor: monitor,
        });
    });
}

/// The value of `key` as `ty`, or NULL if the file has no usable value for it.
#[no_mangle]
pub extern "C" fn rsimgv_store_read(key: *const std::ffi::c_char, ty: *const glib::ffi::GVariantType) -> *mut glib::ffi::GVariant {
    if key.is_null() || ty.is_null() {
        return std::ptr::null_mut();
    }
    let key = unsafe { std::ffi::CStr::from_ptr(key) }.to_string_lossy();
    let ty = unsafe { glib::VariantTy::from_ptr(ty) };
    with_store(|store| {
        store
            .values
            .get(short_key(&key))
            .and_then(|value| from_yaml(value, ty))
            .map(|variant| variant.into_glib_ptr())
            .unwrap_or(std::ptr::null_mut())
    })
    .unwrap_or(std::ptr::null_mut())
}

/// Stores `value` under `key` and saves the file. The value is borrowed from the caller.
#[no_mangle]
pub extern "C" fn rsimgv_store_write(key: *const std::ffi::c_char, value: *mut glib::ffi::GVariant) -> i32 {
    if key.is_null() || value.is_null() {
        return 0;
    }
    let key = unsafe { std::ffi::CStr::from_ptr(key) }.to_string_lossy().into_owned();
    let variant = unsafe { glib::Variant::from_glib_none(value) };
    let saved = with_store(|store| {
        store.values.insert(short_key(&key).to_owned(), to_yaml(&variant));
        save(&store.file, &store.values).is_ok()
    })
    .unwrap_or(false);
    i32::from(saved)
}

/// Removes `key` so its schema default applies again.
#[no_mangle]
pub extern "C" fn rsimgv_store_reset(key: *const std::ffi::c_char) {
    if key.is_null() {
        return;
    }
    let key = unsafe { std::ffi::CStr::from_ptr(key) }.to_string_lossy().into_owned();
    with_store(|store| {
        store.values.remove(short_key(&key));
        let _ = save(&store.file, &store.values);
    });
}

/// Saves any pending settings. Writes already save as they happen, so this has nothing to do.
#[no_mangle]
pub extern "C" fn rsimgv_store_sync() {}
