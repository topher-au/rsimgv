//! GSettings for the application, stored as YAML in `~/.config/rsimgv/rsimgv.conf` (or `RSIMGV_CONFIG`).
//!
//! The backend is an object the application creates and gives to `gio::Settings` itself, so nothing is selected by the
//! environment or registered with GIO. `storage` is the file format and `backend` the GSettingsBackend behind it, whose
//! C subclass is native/backend.c.
//!
//! The file nests each setting by its key path below the schema's path, to any depth: a setting at
//! `/au/topher/rsimgv/window/sidebar/width` is saved as `width` under `window` and `sidebar`. Each group is a child schema
//! in `data/au.topher.rsimgv.gschema.xml`, and code names a setting by its dotted path, `window.sidebar.width`, through
//! `tree::Settings`.

mod backend;
#[cfg(test)]
mod tests;
mod storage;
mod tree;

use std::cell::RefCell;
use std::path::PathBuf;

use anyhow::Context;
use gtk::{gio, glib};

pub use backend::YamlSettingsBackend;
pub use tree::{ChangedHandler, GuardHandle, Settings};

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
pub fn open() -> anyhow::Result<Settings> {
    let schema = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(SCHEMA_ID, true))
        .with_context(|| format!("the settings schema {SCHEMA_ID} is not installed"))?;
    let root = schema.path().with_context(|| format!("the settings schema {SCHEMA_ID} has no path"))?;
    let backend = YamlSettingsBackend::new(config_path(), &root)?;
    let settings = Settings::new(backend.settings(&schema, None));
    check_schemas(&settings, &root)?;
    APP.with(|app| *app.borrow_mut() = Some(App { backend, schema }));
    Ok(settings)
}

/// The schema this source ships, embedded so the installed one can be checked against it.
const SHIPPED_SCHEMA: &str = include_str!("../../data/au.topher.rsimgv.gschema.xml");

/// Checks that `settings` has every group the application's settings ship with. An installed schema older than the
/// binary lacks the groups added since, and the first setting read from one would panic; failing here instead says
/// what is wrong and how to fix it.
fn check_schemas(settings: &Settings, root: &str) -> anyhow::Result<()> {
    for line in SHIPPED_SCHEMA.lines() {
        let Some(tag) = line.trim().strip_prefix("<schema ") else { continue };
        let (Some(id), Some(path)) = (attribute(tag, "id=\""), attribute(tag, "path=\"")) else { continue };
        let group = path.strip_prefix(root).and_then(|below| below.strip_suffix('/')).map(|below| below.replace('/', ".")).unwrap_or_default();
        anyhow::ensure!(
            settings.has_group(&group),
            "the installed settings schema {id} is older than this build: it has no {group} group; reinstall rsimgv"
        );
    }
    Ok(())
}

/// The value of the attribute `name`, such as `id="`, in the rest of a `<schema` tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    tag.split_once(name)?.1.split_once('"').map(|(value, _)| value)
}

/// New `Settings` for the application's schema, on the backend `open` created.
///
/// Panics if `open` has not been called on this thread.
pub fn app_settings() -> Settings {
    APP.with(|app| {
        let app = app.borrow();
        let app = app.as_ref().expect("settings::open runs before any settings are used");
        Settings::new(app.backend.settings(&app.schema, None))
    })
}
