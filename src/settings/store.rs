//! YAML storage for the `yaml` GSettings backend (see backend.c).
//!
//! The file is `~/.config/rsimgv/rsimgv.conf` unless `RSIMGV_CONFIG` names another path. Settings are
//! grouped by what they belong to, and each setting is named within its group:
//!
//! ```yaml
//! window:
//!   width: 960
//!   height: 640
//!   maximized: false
//! browse:
//!   item-size: 128
//!   media-only: true
//! ```
//!
//! A name with dots in it is kept in nested mappings, so `thumbnail-bar.size` in `view` is saved as:
//!
//! ```yaml
//! view:
//!   thumbnail-bar:
//!     size: 80
//! ```
//!
//! A file in the older layout, with every setting under one `settings` mapping, still loads.
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

/// Where each setting is kept in the file: its schema key, then its group and its name in that group.
const LAYOUT: &[(&str, &str, &str)] = &[
    ("show-hidden", "browse", "show-hidden"),
    ("media-only", "browse", "media-only"),
    ("item-size", "browse", "item-size"),
    ("filename-display", "browse", "filename-display"),
    ("show-directories", "browse", "show-directories"),
    ("info-fields", "browse", "info-fields"),
    ("info-layout", "browse", "info-layout"),
    ("collapse-on-navigate", "tree", "collapse-on-navigate"),
    ("window-width", "window", "width"),
    ("window-height", "window", "height"),
    ("window-maximized", "window", "maximized"),
    ("sidebar-width", "window", "sidebar-width"),
    ("start-location", "general", "start-location"),
    ("start-directory", "general", "start-directory"),
    ("last-directory", "general", "last-directory"),
    ("last-file", "general", "last-file"),
    ("open-fullscreen", "general", "open-fullscreen"),
    ("auto-hide-cursor", "general", "auto-hide-cursor"),
    ("smooth-scrolling", "general", "smooth-scrolling"),
    ("image-info-in-title", "general", "image-info-in-title"),
    ("zoom-indicator", "general", "zoom-indicator"),
    ("image-fit", "view", "image-fit"),
    ("keep-fit-mode", "view", "keep-fit-mode"),
    ("expand-up-to-factor", "view", "expand-up-to-factor"),
    ("zoom-step", "view", "zoom-step"),
    ("video-backend", "view", "video-backend"),
    ("mpv-args", "view", "mpv-args"),
    ("scaling-filter", "view", "scaling-filter"),
    ("smooth-upscaling", "view", "smooth-upscaling"),
    ("viewer-blurred-background", "view", "blurred-background"),
    ("viewer-thumbnail-bar", "view", "thumbnail-bar.side"),
    ("viewer-thumbnail-bar-shown", "view", "thumbnail-bar.shown"),
    ("viewer-thumbnail-bar-size", "view", "thumbnail-bar.size"),
    ("viewer-thumbnail-bar-overlay", "view", "thumbnail-bar.overlay"),
    ("viewer-thumbnail-bar-pinned", "view", "thumbnail-bar.pinned"),
    ("ipc-socket", "ipc", "socket"),
    ("ipc-tcp-port", "ipc", "tcp-port"),
    ("ipc-http-port", "ipc", "http-port"),
];

/// The group for a setting that is not in `LAYOUT`. Its name is the schema key.
const FALLBACK_GROUP: &str = "settings";

/// The group and name a schema key is saved under.
fn location(key: &str) -> (String, String) {
    LAYOUT
        .iter()
        .find(|(schema_key, _, _)| *schema_key == key)
        .map_or_else(
            || (FALLBACK_GROUP.to_owned(), key.to_owned()),
            |(_, group, name)| ((*group).to_owned(), (*name).to_owned()),
        )
}

/// The schema key stored under `group` and `name`, if there is one.
fn schema_key(group: &str, name: &str) -> Option<String> {
    LAYOUT
        .iter()
        .find(|(_, layout_group, layout_name)| *layout_group == group && *layout_name == name)
        .map(|(key, _, _)| (*key).to_owned())
        .or_else(|| (group == FALLBACK_GROUP).then(|| name.to_owned()))
}

/// Reads the settings from `file`. A missing or empty file has no settings.
fn load(file: &Path) -> BTreeMap<String, Value> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    let root: Value = serde_yaml_ng::from_str(&text).unwrap_or(Value::Null);
    let mut values = BTreeMap::new();
    let Some(groups) = root.as_mapping() else {
        return values;
    };
    for (group, group_value) in groups {
        let (Some(group), Some(names)) = (group.as_str(), group_value.as_mapping()) else {
            continue;
        };
        for (name, value) in flatten(names) {
            if let Some(key) = schema_key(group, &name) {
                values.insert(key, value);
            }
        }
    }
    values
}

/// The settings in a group's mapping, with the names of those in nested mappings joined by dots.
fn flatten(names: &Mapping) -> Vec<(String, Value)> {
    let mut flat = Vec::new();
    for (name, value) in names {
        let Some(name) = name.as_str() else { continue };
        match value {
            Value::Mapping(inner) => {
                flat.extend(flatten(inner).into_iter().map(|(inner_name, value)| (format!("{name}.{inner_name}"), value)));
            }
            _ => flat.push((name.to_owned(), value.clone())),
        }
    }
    flat
}

/// Puts `value` in `mapping` under `name`, in nested mappings for each part of the name before a dot.
fn insert_nested(mapping: &mut Mapping, name: &str, value: Value) {
    match name.split_once('.') {
        Some((first, rest)) => {
            let first = Value::String(first.to_owned());
            if !matches!(mapping.get(&first), Some(Value::Mapping(_))) {
                mapping.insert(first.clone(), Value::Mapping(Mapping::new()));
            }
            if let Some(Value::Mapping(inner)) = mapping.get_mut(&first) {
                insert_nested(inner, rest, value);
            }
        }
        None => {
            mapping.insert(Value::String(name.to_owned()), value);
        }
    }
}

/// Writes the settings to `file` through a temporary file, so a crash never leaves it half-written.
fn save(file: &Path, values: &BTreeMap<String, Value>) -> std::io::Result<()> {
    let mut root = Mapping::new();
    for (key, value) in values {
        let (group, name) = location(key);
        let group_key = Value::String(group);
        if root.get(&group_key).is_none() {
            root.insert(group_key.clone(), Value::Mapping(Mapping::new()));
        }
        if let Some(Value::Mapping(names)) = root.get_mut(&group_key) {
            insert_nested(names, &name, value.clone());
        }
    }
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
        "as" => value
            .as_sequence()?
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
            .map(|list| list.to_variant())
            // Lists saved as GVariant text by earlier versions still load.
            .or_else(|| value.as_str().and_then(|text| glib::Variant::parse(Some(ty), text).ok())),
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
        "as" => value.get::<Vec<String>>().map(|list| list.into_iter().map(Value::String).collect()).map(Value::Sequence),
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Settings are written under their groups, and read back from them.
    pub(crate) fn run_scenarios() {
        let file = std::env::temp_dir().join(format!("rsimgv-layout-{}.conf", std::process::id()));
        let mut values = BTreeMap::new();
        values.insert("window-height".to_owned(), Value::Number(Number::from(640)));
        values.insert("window-width".to_owned(), Value::Number(Number::from(960)));
        values.insert("item-size".to_owned(), Value::Number(Number::from(128)));
        values.insert("show-hidden".to_owned(), Value::Bool(false));
        values.insert("unlisted".to_owned(), Value::Bool(true));
        save(&file, &values).unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        let root: Value = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(root["window"]["height"], Value::Number(Number::from(640)), "window.height");
        assert_eq!(root["window"]["width"], Value::Number(Number::from(960)), "window.width");
        assert_eq!(root["browse"]["item-size"], Value::Number(Number::from(128)), "browse.item-size");
        assert_eq!(root["browse"]["show-hidden"], Value::Bool(false), "browse.show-hidden");
        assert_eq!(root["settings"]["unlisted"], Value::Bool(true), "unlisted keys fall back to settings");

        assert_eq!(load(&file), values, "the file reads back to the same values");
        let _ = std::fs::remove_file(&file);
    }
}

#[cfg(test)]
mod string_list_tests {
    use super::*;

    #[test]
    fn string_lists_are_yaml_sequences() {
        let list = ["size", "modified"].map(str::to_owned).to_variant();
        let yaml = to_yaml(&list);
        assert_eq!(yaml, Value::Sequence(vec![Value::String("size".into()), Value::String("modified".into())]));
        assert_eq!(from_yaml(&yaml, glib::VariantTy::new("as").unwrap()), Some(list));
    }

    #[test]
    fn old_text_lists_still_load() {
        let text = Value::String("['size']".to_owned());
        let loaded = from_yaml(&text, glib::VariantTy::new("as").unwrap());
        assert_eq!(loaded, Some(vec!["size".to_owned()].to_variant()));
    }
}
