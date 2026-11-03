//! The Rust side of the settings backend: the state behind the GSettingsBackend object (native/backend.c), the
//! notifications that tell GSettings what changed, the watch on the file for outside edits, and the `extern "C"`
//! functions the C shim calls.
//!
//! Ownership: the object holds one strong reference to the state, given to it as a raw pointer by
//! `Arc::into_raw`, and releases it with `release` when it is finalized. `YamlSettingsBackend` holds another. The file
//! watch holds only a `Weak`, so the state is not kept alive by its own monitor.
//!
//! Locking: `State::inner` is locked only for in-memory reads and for the write-and-save step of a change. Notifications
//! are sent after the lock is released, since GSettings may call back into the application.
//!
//! Threads: the backend is used from the main thread, which is where GSettings delivers its notifications.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use anyhow::Context;
use gio::prelude::*;
use glib::translate::{FromGlibPtrNone, IntoGlibPtr};
use gtk::{gio, glib};

use super::storage::{self, Document, SchemaKey};

extern "C" {
    /// Creates the GSettingsBackend object around `state`, which it releases with `state_destroy`.
    fn rsimgv_settings_backend_new(
        state: *mut c_void,
        state_destroy: extern "C" fn(*mut c_void),
    ) -> *mut gio::ffi::GSettingsBackend;
}

/// The settings of one file, and what the backend needs to answer GSettings about them.
pub struct State {
    file: PathBuf,
    inner: Mutex<Inner>,
    /// The GSettingsBackend that notifications are sent through. Zero before it is attached and after it is released.
    backend: AtomicUsize,
    /// Watches the file for outside edits. Dropped with the state, which cancels the watch.
    monitor: Mutex<Option<gio::FileMonitor>>,
}

struct Inner {
    document: Document,
    /// The keys of the schemas in use, which decide how the layout before the typed format is read.
    keys: Vec<SchemaKey>,
    /// Whether the file can be written. A file that did not parse when the backend opened is not overwritten.
    writable: bool,
}

impl State {
    /// The state for the settings in `file`. A file that cannot be read is kept as it is, and the settings are read-only.
    pub fn open(file: PathBuf) -> Self {
        let (document, writable) = match storage::load(&file, &[]) {
            Ok(document) => (document, true),
            Err(err) => {
                eprintln!("rsimgv: the settings are read-only until the file is fixed: {err:#}");
                (Document::default(), false)
            }
        };
        Self {
            file,
            inner: Mutex::new(Inner { document, keys: Vec::new(), writable }),
            backend: AtomicUsize::new(0),
            monitor: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock is held leaves the settings as they were before the change, so the lock is used anyway.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records the GSettingsBackend that notifications go through.
    fn attach(&self, backend: *mut gio::ffi::GSettingsBackend) {
        self.backend.store(backend as usize, Ordering::Release);
    }

    /// Stops notifications, before the object that receives them is released.
    fn detach(&self) {
        self.backend.store(0, Ordering::Release);
    }

    /// Adds the schema keys `keys` (GSettings key, name in the schema, GVariant type) and reads the file for them.
    pub fn register(&self, keys: Vec<(String, String, String)>) {
        {
            let mut inner = self.lock();
            for (key, name, ty) in keys {
                if !inner.keys.iter().any(|known| known.path == key) {
                    inner.keys.push(SchemaKey { path: key, name, ty });
                }
            }
        }
        let changed = self.refresh();
        self.notify(&changed, std::ptr::null_mut());
    }

    /// The value of the GSettings key `key` as `ty`. `None` when no setting is saved, or the saved one has another type,
    /// so GSettings uses the schema default.
    pub fn read(&self, key: &str, ty: &glib::VariantTy) -> Option<glib::Variant> {
        let inner = self.lock();
        let variant = inner.document.values.get(key)?;
        if variant.type_() == ty {
            Some(variant.clone())
        } else {
            eprintln!("rsimgv: {key} is saved as {} but read as {}; using the default", variant.type_().as_str(), ty.as_str());
            None
        }
    }

    pub fn is_writable(&self) -> bool {
        self.lock().writable
    }

    /// Saves `changes` as one write: each is a key with its new value, or `None` to remove it. Returns the keys whose value
    /// changed. The file is read again first, so settings that another instance saved since are kept.
    /// Nothing changes in memory unless the file is written.
    fn apply(&self, changes: Vec<(String, Option<glib::Variant>)>) -> anyhow::Result<Vec<String>> {
        let mut inner = self.lock();
        anyhow::ensure!(
            inner.writable,
            "the settings file could not be read when rsimgv started, so it is read-only until it is fixed and rsimgv restarts"
        );
        let mut next = storage::load(&self.file, &inner.keys)?;
        for (path, value) in changes {
            next.invalid.remove(&path);
            match value {
                Some(value) => {
                    next.values.insert(path, value);
                }
                None => {
                    next.values.remove(&path);
                }
            }
        }
        storage::save(&self.file, &next)?;
        let changed = changed_paths(&inner.document.values, &next.values);
        inner.document = next;
        Ok(changed)
    }

    /// Reads the file again and returns the keys whose value changed. A file that no longer parses leaves the
    /// settings as they are. A file that is gone leaves none, so every setting in it falls back to its default.
    fn refresh(&self) -> Vec<String> {
        let mut inner = self.lock();
        let fresh = match storage::load(&self.file, &inner.keys) {
            Ok(fresh) => fresh,
            Err(err) => {
                eprintln!("rsimgv: keeping the current settings: {err:#}");
                return Vec::new();
            }
        };
        let changed = changed_paths(&inner.document.values, &fresh.values);
        inner.document = fresh;
        changed
    }

    /// Tells GSettings that the keys `changed` have new values, with `origin` (the object that made the change, or
    /// null for an outside edit) so that it is not told about its own write.
    fn notify(&self, changed: &[String], origin: *mut c_void) {
        let backend = self.backend.load(Ordering::Acquire) as *mut gio::ffi::GSettingsBackend;
        if backend.is_null() || changed.is_empty() {
            return;
        }
        // GSettings takes a path and the names under it, so keys are grouped by the path they are in.
        let mut groups: BTreeMap<String, Vec<CString>> = BTreeMap::new();
        for key in changed {
            let Some(split) = key.rfind('/') else { continue };
            let (path, name) = key.split_at(split + 1);
            if let Ok(name) = CString::new(name) {
                groups.entry(path.to_owned()).or_default().push(name);
            }
        }
        for (path, names) in groups {
            let Ok(path) = CString::new(path) else { continue };
            let mut items: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
            items.push(std::ptr::null());
            unsafe {
                gio::ffi::g_settings_backend_keys_changed(backend, path.as_ptr(), items.as_ptr(), origin);
            }
        }
    }

    /// Reads the file again after an outside edit, and tells GSettings about every setting that changed.
    fn reload(&self) {
        let changed = self.refresh();
        self.notify(&changed, std::ptr::null_mut());
    }
}

/// The keys whose value differs between `old` and `new`, where a value that is missing counts as different.
fn changed_paths(old: &BTreeMap<String, glib::Variant>, new: &BTreeMap<String, glib::Variant>) -> Vec<String> {
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    paths.into_iter().filter(|path| old.get(*path) != new.get(*path)).cloned().collect()
}

/// Watches the file of `state`, so an edit made outside the application is picked up.
pub fn watch(state: &Arc<State>) {
    let file = gio::File::for_path(&state.file);
    match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
        Ok(monitor) => {
            let weak: Weak<State> = Arc::downgrade(state);
            monitor.connect_changed(move |_, _, _, event| {
                // A write is read once it is finished, and a file that is gone or recreated is read as it is then.
                let relevant = matches!(
                    event,
                    gio::FileMonitorEvent::ChangesDoneHint
                        | gio::FileMonitorEvent::MovedIn
                        | gio::FileMonitorEvent::Created
                        | gio::FileMonitorEvent::Deleted
                );
                if relevant {
                    if let Some(state) = weak.upgrade() {
                        state.reload();
                    }
                }
            });
            *state.monitor.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(monitor);
        }
        Err(err) => eprintln!("rsimgv: outside edits to {} are not picked up: {err}", state.file.display()),
    }
}

/// Creates the GSettingsBackend object for `state`, and attaches it. The state is shared with the object, which releases
/// its reference in `release`.
pub fn create_backend(state: &Arc<State>) -> gio::SettingsBackend {
    let raw = unsafe { rsimgv_settings_backend_new(Arc::into_raw(Arc::clone(state)) as *mut c_void, release) };
    state.attach(raw);
    unsafe { glib::translate::from_glib_full(raw) }
}

/// Called by the object when it is finalized, with the reference it was given.
extern "C" fn release(state: *mut c_void) {
    if state.is_null() {
        return;
    }
    let state = unsafe { Arc::from_raw(state as *const State) };
    state.detach();
    drop(state);
}

/// Runs `body`, and returns `fallback` if it panics, so that no panic unwinds across the C calls into here.
fn guarded<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| {
        eprintln!("rsimgv: the settings backend failed unexpectedly");
        fallback
    })
}

/// The state behind `pointer`, which the object gave out. It is only borrowed, so the object keeps its reference.
unsafe fn state_ref<'a>(pointer: *mut c_void) -> Option<&'a State> {
    (!pointer.is_null()).then(|| &*(pointer as *const State))
}

/// The string at `pointer`, or `None` if it is null.
unsafe fn string_at(pointer: *const c_char) -> Option<String> {
    (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_string_lossy().into_owned())
}

#[no_mangle]
pub extern "C" fn rsimgv_state_read(
    state: *mut c_void,
    key: *const c_char,
    ty: *const glib::ffi::GVariantType,
) -> *mut glib::ffi::GVariant {
    guarded(std::ptr::null_mut(), || {
        let (Some(state), Some(key)) = (unsafe { state_ref(state) }, unsafe { string_at(key) }) else {
            return std::ptr::null_mut();
        };
        if ty.is_null() {
            return std::ptr::null_mut();
        }
        let ty = unsafe { glib::VariantTy::from_ptr(ty) };
        // The result is a new reference, which GSettings takes ownership of.
        state.read(&key, ty).map_or(std::ptr::null_mut(), |variant| variant.into_glib_ptr())
    })
}

#[no_mangle]
pub extern "C" fn rsimgv_state_get_writable(state: *mut c_void, _key: *const c_char) -> i32 {
    guarded(0, || i32::from(unsafe { state_ref(state) }.is_some_and(State::is_writable)))
}

#[no_mangle]
pub extern "C" fn rsimgv_state_is_writable(state: *mut c_void) -> i32 {
    guarded(0, || i32::from(unsafe { state_ref(state) }.is_some_and(State::is_writable)))
}

/// Saves `value`, which is borrowed, under `key`. Returns 1 if it was saved.
#[no_mangle]
pub extern "C" fn rsimgv_state_write(
    state: *mut c_void,
    key: *const c_char,
    value: *mut glib::ffi::GVariant,
    origin: *mut c_void,
) -> i32 {
    guarded(0, || {
        let (Some(state), Some(key)) = (unsafe { state_ref(state) }, unsafe { string_at(key) }) else {
            return 0;
        };
        if value.is_null() {
            return 0;
        }
        // from_glib_none takes its own reference, so the borrowed value stays with GSettings.
        let value = unsafe { glib::Variant::from_glib_none(value) };
        match state.apply(vec![(key.clone(), Some(value))]) {
            Ok(changed) => {
                state.notify(&changed, origin);
                1
            }
            Err(err) => {
                eprintln!("rsimgv: could not save {key}: {err:#}");
                0
            }
        }
    })
}

/// Saves a batch of values as one write: `keys` are relative to `path` and `values` are borrowed. Both arrays are
/// NULL-terminated. Nothing is saved unless every value is.
#[no_mangle]
pub extern "C" fn rsimgv_state_write_tree(
    state: *mut c_void,
    path: *const c_char,
    keys: *const *const c_char,
    values: *const *mut glib::ffi::GVariant,
    origin: *mut c_void,
) -> i32 {
    guarded(0, || {
        let (Some(state), Some(base)) = (unsafe { state_ref(state) }, unsafe { string_at(path) }) else {
            return 0;
        };
        if keys.is_null() || values.is_null() {
            return 0;
        }
        let mut changes = Vec::new();
        for index in 0.. {
            let (key, value) = unsafe { (*keys.add(index), *values.add(index)) };
            if key.is_null() {
                break;
            }
            let Some(name) = (unsafe { string_at(key) }) else { return 0 };
            if value.is_null() {
                return 0;
            }
            let value = unsafe { glib::Variant::from_glib_none(value) };
            changes.push((format!("{base}{name}"), Some(value)));
        }
        match state.apply(changes) {
            Ok(changed) => {
                state.notify(&changed, origin);
                1
            }
            Err(err) => {
                eprintln!("rsimgv: could not save the changes under {base}: {err:#}");
                0
            }
        }
    })
}

/// Removes the value of `key`, so its schema default applies again.
#[no_mangle]
pub extern "C" fn rsimgv_state_reset(state: *mut c_void, key: *const c_char, origin: *mut c_void) {
    guarded((), || {
        let (Some(state), Some(key)) = (unsafe { state_ref(state) }, unsafe { string_at(key) }) else {
            return;
        };
        match state.apply(vec![(key.clone(), None)]) {
            Ok(changed) => state.notify(&changed, origin),
            Err(err) => eprintln!("rsimgv: could not reset {key}: {err:#}"),
        }
    })
}

/// Nothing to do: every change is saved as it is made.
#[no_mangle]
pub extern "C" fn rsimgv_state_sync(_state: *mut c_void) {}

/// A settings file, and the GSettingsBackend that gives its settings to `gio::Settings`. Keep it alive for as long as
/// its settings are used: the file is watched only while it is.
pub struct YamlSettingsBackend {
    state: Arc<State>,
    backend: gio::SettingsBackend,
}

impl YamlSettingsBackend {
    /// The backend for the settings in `file`, which is created with its parent directory if that is missing. A file
    /// that does not parse does not fail here: the settings are read-only until it is fixed.
    pub fn new(file: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let file = file.into();
        if let Some(parent) = file.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
        }
        let state = Arc::new(State::open(file));
        let backend = create_backend(&state);
        watch(&state);
        Ok(Self { state, backend })
    }

    /// Settings for `schema`, saved in this backend's file. `path` overrides the schema's path, as for a relocatable
    /// schema. Settings of the schema that the file holds in the layout before the typed format are read as well.
    pub fn settings(&self, schema: &gio::SettingsSchema, path: Option<&str>) -> gio::Settings {
        let prefix = path
            .map(str::to_owned)
            .or_else(|| schema.path().map(|path| path.to_string()))
            .unwrap_or_default();
        let keys = schema
            .list_keys()
            .into_iter()
            .map(|name| {
                let name = name.to_string();
                let ty = schema.key(&name).value_type().as_str().to_owned();
                (format!("{prefix}{name}"), name, ty)
            })
            .collect();
        self.state.register(keys);
        gio::Settings::new_full(schema, Some(&self.backend), path)
    }
}
