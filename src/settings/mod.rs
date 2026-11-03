//! GSettings for the application, stored as YAML in `~/.config/rsimgv/rsimgv.conf` (or `RSIMGV_CONFIG`).
//!
//! The backend is an object the application creates and gives to `gio::Settings` itself, so nothing is selected by the
//! environment or registered with GIO. `storage` is the file format and `backend` the GSettingsBackend behind it, whose
//! C subclass is native/backend.c.
//!
//! The file nests each setting by its key path, to any depth: a setting at `/au/topher/rsimgv/group/parent/child` is
//! saved under `au`, `topher`, `rsimgv`, `group` and `parent`. GSettings names cannot contain `--`, so the depth comes
//! from the path: open a relocatable schema at the nested path, such as `/au/topher/rsimgv/group/parent/`.

mod backend;
mod legacy;
#[cfg(test)]
mod tests;
mod storage;

use std::cell::RefCell;
use std::path::PathBuf;

use anyhow::Context;
use gtk::{gio, glib};

pub use backend::YamlSettingsBackend;

/// The schema of the application's settings.
pub const SCHEMA_ID: &str = "au.topher.rsimgv";

/// Where the settings file is: `RSIMGV_CONFIG` if set, otherwise `~/.config/rsimgv/rsimgv.conf`.
pub fn config_path() -> PathBuf {
    std::env::var_os("RSIMGV_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::user_config_dir().join("rsimgv").join("rsimgv.conf"))
}

/// The application's backend and schema, kept for `app_settings`. Only the main thread uses settings.
struct App {
    backend: YamlSettingsBackend,
    schema: gio::SettingsSchema,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Opens the application's settings in `config_path()`, and returns them. Call this once, before any other settings
/// are read; `app_settings` gives more objects for the same file.
pub fn open() -> anyhow::Result<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(SCHEMA_ID, true))
        .with_context(|| format!("the settings schema {SCHEMA_ID} is not installed"))?;
    let backend = YamlSettingsBackend::new(config_path())?;
    let settings = backend.settings(&schema, None);
    APP.with(|app| *app.borrow_mut() = Some(App { backend, schema }));
    Ok(settings)
}

/// A new `gio::Settings` for the application's schema, on the backend `open` created.
///
/// Panics if `open` has not been called on this thread.
pub fn app_settings() -> gio::Settings {
    APP.with(|app| {
        let app = app.borrow();
        let app = app.as_ref().expect("settings::open runs before any settings are used");
        app.backend.settings(&app.schema, None)
    })
}
