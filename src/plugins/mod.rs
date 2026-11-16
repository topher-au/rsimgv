//! The plugin manager: discovery, Python loading, contributions, events, panels and hot reload.
//!
//! Plugins are trusted extensions. The Python backend embeds CPython through PyO3 and builds widgets through
//! PyGObject; see `PLUGINS_PYTHON.md` for the design.

mod manifest;

#[cfg(feature = "python")]
mod bridge;
#[cfg(feature = "python")]
pub mod python;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use serde_json::{json, Value};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::keybindings::actions::{self, Scope};
use crate::settings::Settings;
use crate::ui::Window;

pub use manifest::Manifest;

/// The icon the keybinding editor shows for plugin actions.
const PLUGIN_ICON: &str = "application-x-addon-symbolic";

struct Panel {
    window: glib::WeakRef<Window>,
    panel_id: String,
    instance_id: u64,
    container: gtk::Box,
    /// The viewer's overlay, for a panel in the `viewer.overlay` slot; a sidebar panel has none.
    overlay: Option<gtk::Overlay>,
}

/// Whether the window has a place for panels in `slot`. The other slots are accepted in a manifest but not built yet.
fn builds_slot(slot: &str) -> bool {
    matches!(slot, "sidebar.bottom" | "browse.sidebar" | "viewer.overlay")
}

struct Entry {
    manifest: Manifest,
    dir: PathBuf,
    enabled: bool,
    error: Option<String>,
    #[cfg(feature = "python")]
    python: Option<python::PythonPlugin>,
    panels: Vec<Panel>,
    modified: Option<SystemTime>,
    generation: u64,
}

#[cfg(feature = "python")]
struct Signal {
    plugin: String,
    widget: pyo3::Py<pyo3::PyAny>,
    id: u64,
}

struct Manager {
    app: Option<adw::Application>,
    entries: Vec<Entry>,
    next_instance: u64,
    events: Vec<(String, Value)>,
    #[cfg(feature = "python")]
    signals: Vec<Signal>,
    #[cfg(feature = "python")]
    sources: Vec<(String, u32)>,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            app: None,
            entries: Vec::new(),
            next_instance: 1,
            events: Vec::new(),
            #[cfg(feature = "python")]
            signals: Vec::new(),
            #[cfg(feature = "python")]
            sources: Vec::new(),
        }
    }
}

thread_local! {
    static MANAGER: RefCell<Manager> = RefCell::new(Manager::default());
    /// The result of the plugin action an IPC call is waiting for.
    #[cfg(feature = "python")]
    static PENDING_ACTION: RefCell<Option<Result<Value, String>>> = const { RefCell::new(None) };
    /// The nested main loop an IPC call runs while it waits for a plugin action.
    #[cfg(feature = "python")]
    static ACTION_WAIT: RefCell<Option<glib::MainLoop>> = const { RefCell::new(None) };
}

/// Clones a Python object with the GIL held. Used when a stored plugin instance has to outlive a manager
/// borrow while a callback runs.
#[cfg(feature = "python")]
fn clone_py(value: &Py<PyAny>) -> Py<PyAny> {
    Python::attach(|py| value.clone_ref(py))
}

/// Queued calls from Python worker threads. Worker threads have their own thread-locals, so this queue is a
/// process-wide mutex rather than part of the thread-local manager.
#[cfg(feature = "python")]
static IDLE: std::sync::Mutex<Vec<pyo3::Py<pyo3::PyAny>>> = std::sync::Mutex::new(Vec::new());

/// Where plugins are looked for, in order. `RSIMGV_PLUGINS` is a colon-separated list, which is useful while
/// developing without installing. The last entries come from the `plugins.extra-directories` setting.
pub fn search_dirs(settings: &Settings) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(paths) = std::env::var_os("RSIMGV_PLUGINS") {
        dirs.extend(std::env::split_paths(&paths));
    }
    dirs.push(glib::user_data_dir().join("rsimgv").join("plugins"));
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| std::env::split_paths(&dirs).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);
    for dir in data_dirs {
        dirs.push(dir.join("rsimgv").join("plugins"));
    }
    for dir in settings.strv("plugins.extra-directories").iter() {
        dirs.push(PathBuf::from(dir.as_str()));
    }
    dirs
}

/// Reads every manifest and registers the actions they declare. This runs before the keybinding table is first
/// used, so plugin defaults are part of it.
pub fn discover(settings: &Settings) {
    let enabled_ids: Vec<String> = settings.strv("plugins.enabled").iter().map(|id| id.to_string()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();

    for root in search_dirs(settings) {
        let candidates = plugin_dirs(&root);
        for dir in candidates {
            let manifest_path = dir.join("plugin.yaml");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = match Manifest::load(&manifest_path, &dir) {
                Ok(manifest) => manifest,
                Err(err) => {
                    eprintln!("rsimgv: plugin {}: {err}", manifest_path.display());
                    continue;
                }
            };
            if !seen.insert(manifest.id.clone()) {
                eprintln!("rsimgv: plugin {} appears more than once; using the first", manifest.id);
                continue;
            }
            for action in &manifest.contributes.actions {
                let scope = Scope::from_id(&action.scope).unwrap_or(Scope::Global);
                let name = manifest.action_name(&action.id);
                actions::register_plugin_action(
                    &name,
                    &action.label,
                    &action.description,
                    scope,
                    PLUGIN_ICON,
                    &action.default_keys,
                );
            }
            let enabled = enabled_ids.contains(&manifest.id);
            let modified = modified_time(&dir);
            found.push(Entry {
                manifest,
                dir,
                enabled,
                error: None,
                #[cfg(feature = "python")]
                python: None,
                panels: Vec::new(),
                modified,
                generation: 1,
            });
        }
    }

    MANAGER.with(|manager| manager.borrow_mut().entries = found);
    collect_menu_items();
}

/// The plugin directories below `root`: `root` itself when it holds a manifest, and each child that does.
fn plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if root.join("plugin.yaml").is_file() {
        dirs.push(root.to_path_buf());
        return dirs;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return dirs };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("plugin.yaml").is_file() {
            dirs.push(path);
        }
    }
    dirs.sort();
    dirs
}

/// Starts the interpreter and loads every enabled plugin. Runs on the GTK main thread during application startup.
pub fn start(app: &adw::Application, settings: &Settings) {
    MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        manager.app = Some(app.clone());
    });

    #[cfg(feature = "python")]
    if let Err(err) = python::init() {
        eprintln!("rsimgv: Python plugins are unavailable: {err}");
    }

    load_all();
    collect_menu_items();
    start_event_pump();
    start_watcher(settings.clone());
}

fn load_all() {
    let indices: Vec<usize> = MANAGER.with(|manager| {
        manager
            .borrow()
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.enabled && entry.manifest.startup == manifest::Startup::Always)
            .map(|(index, _)| index)
            .collect()
    });
    for index in indices {
        load_entry(index);
    }
}

fn load_entry(index: usize) {
    let (enabled, runtime) = MANAGER.with(|manager| {
        let manager = manager.borrow();
        let entry = &manager.entries[index];
        (entry.enabled, entry.manifest.runtime)
    });
    if !enabled {
        return;
    }

    match runtime {
        manifest::Runtime::Python => {
            #[cfg(feature = "python")]
            {
                let loaded = MANAGER.with(|manager| {
                    let manager = manager.borrow();
                    let entry = &manager.entries[index];
                    python::load_plugin(&entry.manifest, &entry.dir, entry.generation)
                });
                MANAGER.with(|manager| {
                    let mut manager = manager.borrow_mut();
                    let entry = &mut manager.entries[index];
                    match loaded {
                        Ok(plugin) => {
                            entry.python = Some(plugin);
                            entry.error = None;
                        }
                        Err(err) => {
                            eprintln!("rsimgv: plugin {}: {err}", entry.manifest.id);
                            entry.python = None;
                            entry.error = Some(err);
                        }
                    }
                });
            }
            #[cfg(not(feature = "python"))]
            MANAGER.with(|manager| {
                manager.borrow_mut().entries[index].error = Some("this build has no Python support".to_owned());
            });
        }
        manifest::Runtime::Native => {
            MANAGER.with(|manager| {
                manager.borrow_mut().entries[index].error = Some("native plugins are not supported by this build".to_owned());
            });
        }
    }
}

/// Builds the panels a window has a slot for. Called when a window is made and after a reload.
pub fn build_panels(window: &Window) {
    add_panels(window, None);
}

/// Builds the sidebar panels of the plugin at `only`, or of every running plugin when `only` is `None`. Panels
/// already in the window are not checked, so a caller adds only the plugins that have none yet.
fn add_panels(window: &Window, only: Option<usize>) {
    #[cfg(feature = "python")]
    {
        // A plugin with a panel is needed as soon as a window exists, even when it is on-demand.
        let lazy: Vec<usize> = MANAGER.with(|manager| {
            manager
                .borrow()
                .entries
                .iter()
                .enumerate()
                .filter(|(index, entry)| {
                    only.is_none_or(|wanted| wanted == *index)
                        && entry.enabled
                        && entry.error.is_none()
                        && entry.python.is_none()
                        && entry.manifest.contributes.panels.iter().any(|panel| builds_slot(&panel.slot))
                })
                .map(|(index, _)| index)
                .collect()
        });
        for index in lazy {
            load_entry(index);
        }

        let mut work = Vec::new();
        MANAGER.with(|manager| {
            let mut manager = manager.borrow_mut();
            for index in 0..manager.entries.len() {
                if only.is_some_and(|wanted| wanted != index) || manager.entries[index].python.is_none() {
                    continue;
                }
                let panels: Vec<(String, String, String, String)> = manager.entries[index]
                    .manifest
                    .contributes
                    .panels
                    .iter()
                    .filter(|panel| builds_slot(&panel.slot))
                    .map(|panel| (panel.id.clone(), panel.title.clone(), panel.slot.clone(), panel.position.clone()))
                    .collect();
                for (panel_id, title, slot, position) in panels {
                    let instance_id = manager.next_instance;
                    manager.next_instance += 1;
                    let container = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    container.add_css_class("rsimgv-plugin-panel");
                    if !title.is_empty() {
                        let label = gtk::Label::new(Some(&title));
                        label.set_xalign(0.0);
                        label.add_css_class("caption-heading");
                        container.append(&label);
                    }
                    let overlay = if slot == "viewer.overlay" {
                        // Over the picture, in the corner the manifest names (the top right by default). The viewer
                        // clips the overlay to the item, so the panel stays within it.
                        let overlay = window.viewer_overlay();
                        container.add_css_class("card");
                        container.add_css_class("background");
                        container.set_margin_top(12);
                        container.set_margin_bottom(12);
                        container.set_margin_start(12);
                        container.set_margin_end(12);
                        let (horizontal, vertical) = match position.as_str() {
                            "top-left" => (gtk::Align::Start, gtk::Align::Start),
                            "bottom-left" => (gtk::Align::Start, gtk::Align::End),
                            "bottom-right" => (gtk::Align::End, gtk::Align::End),
                            _ => (gtk::Align::End, gtk::Align::Start),
                        };
                        container.set_halign(horizontal);
                        container.set_valign(vertical);
                        overlay.add_overlay(&container);
                        Some(overlay)
                    } else if slot == "browse.sidebar" {
                        // The chat sidebar is open while a panel in it is shown, so it follows the panel's visibility.
                        window.browse_sidebar_host().append(&container);
                        container.connect_visible_notify({
                            let weak = window.downgrade();
                            move |_| {
                                if let Some(window) = weak.upgrade() {
                                    window.update_chat_sidebar();
                                }
                            }
                        });
                        window.update_chat_sidebar();
                        None
                    } else {
                        window.plugin_panel_host().append(&container);
                        None
                    };
                    manager.entries[index].panels.push(Panel {
                        window: window.downgrade(),
                        panel_id: panel_id.clone(),
                        instance_id,
                        container: container.clone(),
                        overlay,
                    });
                    work.push((index, panel_id, instance_id, container));
                }
            }
        });

        for (index, panel_id, instance_id, container) in work {
            let (instance, id, generation) = MANAGER.with(|manager| {
                let manager = manager.borrow();
                let entry = &manager.entries[index];
                (
                    entry.python.as_ref().map(|plugin| clone_py(&plugin.instance)),
                    entry.manifest.id.clone(),
                    entry.python.as_ref().map_or(0, |plugin| plugin.generation),
                )
            });
            if let Some(instance) = instance {
                let context = panel_context(window);
                if let Err(err) =
                    python::create_panel(&instance, &id, generation, &panel_id, instance_id, &container, &context)
                {
                    eprintln!("rsimgv: plugin {id}: {err}");
                }
            }
        }
    }
}

/// Adds the plugin contributions to the window's main menu. The section is appended each time; callers remove
/// the previous section through the window's stored index.
pub fn build_menu(window: &Window) {
    let Some(menu) = window.plugin_menu() else { return };
    if let Some(index) = window.plugin_menu_section() {
        menu.remove(index as i32);
    }

    let section = gio::Menu::new();
    let mut added = 0;
    for (name, label) in MENU_ITEMS.with(|items| items.borrow().clone()) {
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_detailed_action(&format!("win.plugin-action::{name}"));
        section.append_item(&item);
        added += 1;
    }
    if added > 0 {
        let index = menu.n_items() as u32;
        menu.append_section(None, &section);
        window.set_plugin_menu_section(Some(index));
    }
}

thread_local! {
    static MENU_ITEMS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Records the menu contributions for `build_menu`, which runs per window.
fn collect_menu_items() {
    let items = MANAGER.with(|manager| {
        manager
            .borrow()
            .entries
            .iter()
            .filter(|entry| entry.enabled)
            .flat_map(|entry| {
                entry.manifest.contributes.menus.iter().filter(|menu| menu.menu == "main").map(move |menu| {
                    (entry.manifest.action_name(&menu.action), menu.label.clone())
                })
            })
            .collect::<Vec<_>>()
    });
    MENU_ITEMS.with(|stored| *stored.borrow_mut() = items);
}

/// Runs the plugin action `name` for `window`. Returns whether the action belonged to a plugin.
pub fn invoke_action(name: &str, window: &Window) -> bool {
    #[cfg(feature = "python")]
    {
        let lazy = MANAGER.with(|manager| {
            let manager = manager.borrow();
            manager.entries.iter().position(|entry| {
                entry.enabled
                    && entry.error.is_none()
                    && entry.python.is_none()
                    && entry
                        .manifest
                        .contributes
                        .actions
                        .iter()
                        .any(|action| entry.manifest.action_name(&action.id) == name)
            })
        });
        if let Some(index) = lazy {
            load_entry(index);
        }

        let found = MANAGER.with(|manager| {
            let manager = manager.borrow();
            for entry in &manager.entries {
                if let Some(action) = entry.manifest.contributes.actions.iter().find(|action| entry.manifest.action_name(&action.id) == name) {
                    let Some(plugin) = &entry.python else {
                        let error = if entry.enabled {
                            entry.error.clone()
                        } else {
                            Some(format!("the plugin {} is disabled", entry.manifest.id))
                        };
                        return Some((None, entry.manifest.id.clone(), action.id.clone(), error));
                    };
                    return Some((
                        Some(clone_py(&plugin.instance)),
                        entry.manifest.id.clone(),
                        action.id.clone(),
                        entry.error.clone(),
                    ));
                }
            }
            None
        });
        let Some((instance, id, action, error)) = found else { return false };
        match instance {
            Some(instance) => {
                let generation = MANAGER.with(|manager| {
                    manager
                        .borrow()
                        .entries
                        .iter()
                        .find(|entry| entry.manifest.id == id)
                        .and_then(|entry| entry.python.as_ref().map(|plugin| plugin.generation))
                        .unwrap_or(0)
                });
                let context = action_context(window);
                if let Err(err) = python::invoke_action(&instance, &id, generation, &action, &context) {
                    eprintln!("rsimgv: plugin {id}: {err}");
                }
            }
            None => {
                let message = error.unwrap_or_else(|| format!("the plugin {id} is not running"));
                window.show_notification(&message, "error", 8.0);
            }
        }
        true
    }
    #[cfg(not(feature = "python"))]
    {
        let _ = (name, window);
        false
    }
}

/// Runs a plugin action for an IPC caller and waits for `rsimgv.action_done`. The nested main loop keeps the
/// window responsive and lets the plugin's worker thread finish and post its result.
#[cfg(feature = "python")]
pub fn invoke_action_sync(name: &str, window: &Window, timeout: std::time::Duration) -> Result<Value, String> {
    PENDING_ACTION.with(|pending| *pending.borrow_mut() = None);
    let loop_ = glib::MainLoop::new(None, false);
    ACTION_WAIT.with(|wait| *wait.borrow_mut() = Some(loop_.clone()));

    let timeout_loop = loop_.clone();
    let timeout_id = glib::timeout_add_local_once(timeout, move || {
        PENDING_ACTION.with(|pending| {
            *pending.borrow_mut() = Some(Err("the plugin did not answer in time".to_owned()));
        });
        timeout_loop.quit();
    });

    if !invoke_action(name, window) {
        unsafe { glib::ffi::g_source_remove(timeout_id.as_raw()) };
        ACTION_WAIT.with(|wait| *wait.borrow_mut() = None);
        return Err(format!("no plugin provides {name}"));
    }

    if PENDING_ACTION.with(|pending| pending.borrow().is_none()) {
        loop_.run();
    }

    unsafe { glib::ffi::g_source_remove(timeout_id.as_raw()) };
    ACTION_WAIT.with(|wait| *wait.borrow_mut() = None);
    match PENDING_ACTION.with(|pending| pending.borrow_mut().take()) {
        Some(Ok(Value::Object(map))) if map.contains_key("error") => {
            Err(map.get("error").and_then(Value::as_str).unwrap_or("the plugin reported an error").to_owned())
        }
        Some(Ok(value)) => Ok(value),
        Some(Err(err)) => Err(err),
        None => Err("the plugin did not report a result".to_owned()),
    }
}

/// Called by `rsimgv.action_done`. Ignored unless an IPC call is waiting for a plugin action.
#[cfg(feature = "python")]
pub fn action_done(value: Value) {
    let waiting = ACTION_WAIT.with(|wait| wait.borrow().is_some());
    if !waiting {
        return;
    }
    PENDING_ACTION.with(|pending| *pending.borrow_mut() = Some(Ok(value)));
    ACTION_WAIT.with(|wait| {
        if let Some(loop_) = wait.borrow().as_ref() {
            loop_.quit();
        }
    });
}

#[cfg(not(feature = "python"))]
pub fn invoke_action_sync(_name: &str, _window: &Window, _timeout: std::time::Duration) -> Result<Value, String> {
    Err("this build has no Python support".to_owned())
}

/// Queues an event for plugins. Delivery happens on the main loop, so a plugin cannot re-enter the state change
/// that emitted it.
pub fn queue_event(name: &str, data: Value) {
    MANAGER.with(|manager| manager.borrow_mut().events.push((name.to_owned(), data)));
}

fn action_context(window: &Window) -> Value {
    let viewer = window.viewer_item().map(|item| {
        json!({
            "path": item.path.to_string_lossy(),
            "width": item.width,
            "height": item.height,
        })
    });
    json!({
        "path": window.current_path().map(|path| path.to_string_lossy().into_owned()),
        "selected": window.selected_file(),
        "viewer": viewer,
    })
}

fn panel_context(window: &Window) -> Value {
    action_context(window)
}

/// The context a plugin sees while `host.call` runs. The active window is the same one the IPC uses.
pub fn host_call(method: &str, params: &Value) -> Result<Value, String> {
    let app = MANAGER.with(|manager| manager.borrow().app.clone()).ok_or_else(|| "rsimgv is not running".to_owned())?;
    let window = app
        .active_window()
        .and_downcast::<Window>()
        .ok_or_else(|| "no window is open".to_owned())?;
    crate::services::remote::handle(&window, method, params).map_err(|err| err.message)
}

/// The config file path of plugin `id`, which the plugin may replace with a file beside it.
pub fn config_path(id: &str) -> String {
    let dir = glib::user_config_dir().join("rsimgv").join("plugins").join(id);
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.yaml").to_string_lossy().into_owned()
}

fn config_dir(id: &str) -> PathBuf {
    glib::user_config_dir().join("rsimgv").join("plugins").join(id)
}

fn config_json_path(id: &str) -> PathBuf {
    config_dir(id).join("config.json")
}

/// The settings a plugin exposes in the Plugins page.
pub fn plugin_settings(id: &str) -> Vec<manifest::SettingContribution> {
    MANAGER.with(|manager| {
        manager
            .borrow()
            .entries
            .iter()
            .find(|entry| entry.manifest.id == id)
            .map(|entry| entry.manifest.contributes.settings.clone())
            .unwrap_or_default()
    })
}

/// The current values of a plugin's settings: its declared defaults, overlaid with its `config.json`.
pub fn plugin_config(id: &str) -> Value {
    let mut values = serde_json::Map::new();
    for setting in plugin_settings(id) {
        values.insert(setting.key.clone(), setting.default.clone());
    }
    if let Ok(text) = std::fs::read_to_string(config_json_path(id)) {
        if let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) {
            for (key, value) in saved {
                values.insert(key, value);
            }
        }
    }
    Value::Object(values)
}

/// Writes one setting to the plugin's `config.json` and reloads the plugin so it reads the new value.
pub fn set_plugin_setting(id: &str, key: &str, value: Value) -> Result<(), String> {
    let mut values = match plugin_config(id) {
        Value::Object(values) => values,
        _ => serde_json::Map::new(),
    };
    values.insert(key.to_owned(), value);
    let directory = config_dir(id);
    std::fs::create_dir_all(&directory).map_err(|err| format!("cannot create {}: {err}", directory.display()))?;
    let text = serde_json::to_vec_pretty(&Value::Object(values)).map_err(|err| err.to_string())?;
    crate::storage::atomic_file::write(&config_json_path(id), &text).map_err(|err| err.to_string())?;
    reload(Some(id))
}

/// A list of every discovered plugin, for `plugins.list`.
pub fn list() -> Value {
    MANAGER.with(|manager| {
        let manager = manager.borrow();
        let plugins: Vec<Value> = manager
            .entries
            .iter()
            .map(|entry| {
                #[cfg(feature = "python")]
                let running = entry.python.is_some();
                #[cfg(not(feature = "python"))]
                let running = false;
                json!({
                    "id": entry.manifest.id,
                    "name": entry.manifest.name,
                    "description": entry.manifest.description,
                    "version": entry.manifest.version,
                    "runtime": entry.manifest.runtime.name(),
                    "enabled": entry.enabled,
                    "running": running,
                    "error": entry.error,
                })
            })
            .collect();
        json!({"plugins": plugins})
    })
}

/// Reloads one plugin's Python code, or every plugin when `id` is `None`.
pub fn reload(id: Option<&str>) -> Result<(), String> {
    let ids: Vec<String> = MANAGER.with(|manager| {
        manager
            .borrow()
            .entries
            .iter()
            .filter(|entry| id.is_none_or(|id| id == entry.manifest.id))
            .map(|entry| entry.manifest.id.clone())
            .collect()
    });
    for id in ids {
        reload_one(&id)?;
    }
    Ok(())
}

fn reload_one(id: &str) -> Result<(), String> {
    let index = MANAGER.with(|manager| manager.borrow().entries.iter().position(|entry| entry.manifest.id == id));
    let Some(index) = index else { return Err(format!("no plugin {id}")) };

    // Read and validate the new manifest first: a broken manifest keeps the running plugin.
    let dir = MANAGER.with(|manager| manager.borrow().entries[index].dir.clone());
    let manifest = Manifest::load(&dir.join("plugin.yaml"), &dir).map_err(|err| {
        MANAGER.with(|manager| manager.borrow_mut().entries[index].error = Some(err.clone()));
        err
    })?;

    unload_entry(index);

    MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        manager.entries[index].manifest = manifest;
        manager.entries[index].modified = modified_time(&dir);
        manager.entries[index].generation = manager.entries[index].generation.saturating_add(1);
    });
    load_entry(index);
    collect_menu_items();

    for window in open_windows() {
        build_panels(&window);
        build_menu(&window);
    }
    Ok(())
}

/// Turns a plugin on or off and saves the choice to `plugins.enabled`. Turning a plugin off stops its code and
/// removes its panels and menu items at once; turning it on loads it again where it needs loading.
pub fn set_enabled(id: &str, enabled: bool, settings: &Settings) -> Result<(), String> {
    let index = MANAGER.with(|manager| manager.borrow().entries.iter().position(|entry| entry.manifest.id == id));
    let Some(index) = index else { return Err(format!("no plugin {id}")) };
    let (was_enabled, needs_loading) = MANAGER.with(|manager| {
        let manager = manager.borrow();
        let entry = &manager.entries[index];
        let has_panel = entry.manifest.contributes.panels.iter().any(|panel| builds_slot(&panel.slot));
        (entry.enabled, entry.manifest.startup == manifest::Startup::Always || has_panel)
    });
    if was_enabled == enabled {
        return Ok(());
    }

    let mut enabled_ids: Vec<String> = settings.strv("plugins.enabled").iter().map(|id| id.to_string()).collect();
    enabled_ids.retain(|other| other != id);
    if enabled {
        enabled_ids.push(id.to_owned());
    }
    settings
        .set_strv("plugins.enabled", enabled_ids.as_slice())
        .map_err(|err| format!("cannot save the plugin choice: {err}"))?;

    MANAGER.with(|manager| manager.borrow_mut().entries[index].enabled = enabled);
    if enabled {
        if needs_loading {
            load_entry(index);
        }
    } else {
        unload_entry(index);
        MANAGER.with(|manager| manager.borrow_mut().entries[index].error = None);
    }
    collect_menu_items();

    for window in open_windows() {
        if enabled {
            add_panels(&window, Some(index));
        }
        build_menu(&window);
    }
    Ok(())
}

/// Stops a plugin's Python code and removes what it has put in the windows. The manifest and the actions it
/// declares stay registered, so its keybindings still resolve.
fn unload_entry(index: usize) {
    #[cfg(feature = "python")]
    {
        let (id, instance, module_name) = MANAGER.with(|manager| {
            let manager = manager.borrow();
            let entry = &manager.entries[index];
            (
                entry.manifest.id.clone(),
                entry.python.as_ref().map(|plugin| clone_py(&plugin.instance)),
                entry.python.as_ref().map(|plugin| plugin.module_name.clone()),
            )
        });
        if let Some(instance) = instance {
            if let Err(err) = python::deactivate(&instance) {
                eprintln!("rsimgv: plugin {id}: {err}");
            }
        }
        remove_panels(index);
        disconnect_signals(&id);
        remove_sources(&id);
        if let Some(module_name) = module_name {
            python::purge(&module_name);
        }
        MANAGER.with(|manager| manager.borrow_mut().entries[index].python = None);
    }
    #[cfg(not(feature = "python"))]
    remove_panels(index);
}

/// The application's open windows.
fn open_windows() -> Vec<Window> {
    MANAGER.with(|manager| {
        manager
            .borrow()
            .app
            .as_ref()
            .map(|app| app.windows().into_iter().filter_map(|window| window.downcast::<Window>().ok()).collect())
            .unwrap_or_default()
    })
}

fn remove_panels(index: usize) {
    let panels = MANAGER.with(|manager| std::mem::take(&mut manager.borrow_mut().entries[index].panels));
    for panel in panels {
        #[cfg(feature = "python")]
        {
            let instance = MANAGER.with(|manager| {
                manager.borrow().entries[index].python.as_ref().map(|plugin| clone_py(&plugin.instance))
            });
            if let Some(instance) = instance {
                let _ = python::panel_destroyed(&instance, &panel.panel_id, panel.instance_id);
            }
        }
        match &panel.overlay {
            Some(overlay) => overlay.remove_overlay(&panel.container),
            None => panel.container.unparent(),
        }
        // A chat sidebar with no panel left in it closes.
        if let Some(window) = panel.window.upgrade() {
            window.update_chat_sidebar();
        }
    }
}

fn start_event_pump() {
    glib::timeout_add_local(std::time::Duration::from_millis(50), || {
        let events = MANAGER.with(|manager| std::mem::take(&mut manager.borrow_mut().events));
        for (name, data) in events {
            dispatch_event(&name, &data);
        }
        #[cfg(feature = "python")]
        {
            let callbacks = IDLE.lock().map(|mut idle| std::mem::take(&mut *idle)).unwrap_or_default();
            for callback in callbacks {
                python::run_idle(callback);
            }
        }
        glib::ControlFlow::Continue
    });
}

fn dispatch_event(name: &str, data: &Value) {
    #[cfg(feature = "python")]
    {
        let lazy: Vec<usize> = MANAGER.with(|manager| {
            manager
                .borrow()
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry.enabled
                        && entry.error.is_none()
                        && entry.python.is_none()
                        && entry.manifest.events.iter().any(|event| event == name || event == "*")
                })
                .map(|(index, _)| index)
                .collect()
        });
        for index in lazy {
            load_entry(index);
        }

        let targets = MANAGER.with(|manager| {
            let manager = manager.borrow();
            manager
                .entries
                .iter()
                .filter(|entry| entry.enabled && entry.manifest.events.iter().any(|event| event == name || event == "*"))
                .filter_map(|entry| {
                    entry.python.as_ref().map(|plugin| {
                        (clone_py(&plugin.instance), entry.manifest.id.clone(), plugin.generation)
                    })
                })
                .collect::<Vec<_>>()
        });
        for (instance, id, generation) in targets {
            if let Err(err) = python::on_event(&instance, &id, generation, name, data) {
                eprintln!("rsimgv: plugin {id}: {err}");
            }
        }
    }
    #[cfg(not(feature = "python"))]
    let _ = (name, data);
}

fn start_watcher(settings: Settings) {
    glib::timeout_add_seconds_local(1, move || {
        if settings.boolean("plugins.auto-reload") {
            watch_once();
        }
        glib::ControlFlow::Continue
    });
}

fn watch_once() {
    let changed: Vec<String> = MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let mut changed = Vec::new();
        for entry in &mut manager.entries {
            if !entry.enabled || entry.manifest.runtime != manifest::Runtime::Python {
                continue;
            }
            let modified = modified_time(&entry.dir);
            if modified.is_some() && entry.modified != modified {
                entry.modified = modified;
                changed.push(entry.manifest.id.clone());
            }
        }
        changed
    });
    for id in changed {
        if let Err(err) = reload_one(&id) {
            eprintln!("rsimgv: plugin {id}: reload failed: {err}");
        }
    }
}

fn modified_time(dir: &Path) -> Option<SystemTime> {
    fn walk(dir: &Path, newest: &mut Option<SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "__pycache__") {
                    continue;
                }
                walk(&path, newest);
            } else if path.extension().is_some_and(|ext| ext == "py") || path.file_name().is_some_and(|name| name == "plugin.yaml") {
                if let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) {
                    if newest.is_none_or(|current| modified > current) {
                        *newest = Some(modified);
                    }
                }
            }
        }
    }

    let mut newest = None;
    walk(dir, &mut newest);
    newest
}

/// Stops every plugin, called when the application shuts down.
pub fn shutdown() {
    #[cfg(feature = "python")]
    {
        let instances = MANAGER.with(|manager| {
            manager
                .borrow()
                .entries
                .iter()
                .filter_map(|entry| entry.python.as_ref().map(|plugin| clone_py(&plugin.instance)))
                .collect::<Vec<_>>()
        });
        for instance in instances {
            let _ = python::deactivate(&instance);
        }
    }
    MANAGER.with(|manager| manager.borrow_mut().entries.clear());
}

/// Records a GTK signal connection so a reload can disconnect it. Called from the Python `ui.connect`.
#[cfg(feature = "python")]
pub fn track_signal(plugin: &str, widget: pyo3::Py<pyo3::PyAny>, id: u64) {
    MANAGER.with(|manager| manager.borrow_mut().signals.push(Signal { plugin: plugin.to_owned(), widget, id }));
}

/// Disconnects every tracked signal belonging to `plugin`.
#[cfg(feature = "python")]
pub fn disconnect_signals(plugin: &str) {
    let signals: Vec<Signal> = MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let (ours, rest) = std::mem::take(&mut manager.signals).into_iter().partition(|signal| signal.plugin == plugin);
        manager.signals = rest;
        ours
    });
    pyo3::Python::attach(|py| {
        for signal in signals {
            let _ = signal.widget.bind(py).call_method1("disconnect", (signal.id,));
        }
    });
}

/// Queues a Python callable to run on the GTK main thread. Safe to call from a plugin's worker thread.
#[cfg(feature = "python")]
pub fn queue_idle(callback: pyo3::Py<pyo3::PyAny>) {
    if let Ok(mut idle) = IDLE.lock() {
        idle.push(callback);
    }
}

/// Records a GLib timeout source so a reload can remove it.
#[cfg(feature = "python")]
pub fn track_source(plugin: &str, id: u32) {
    MANAGER.with(|manager| manager.borrow_mut().sources.push((plugin.to_owned(), id)));
}

#[cfg(feature = "python")]
fn remove_sources(plugin: &str) {
    let sources: Vec<u32> = MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let (ours, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut manager.sources).into_iter().partition(|(owner, _)| owner == plugin);
        manager.sources = rest;
        ours.into_iter().map(|(_, id)| id).collect()
    });
    for id in sources {
        unsafe { gtk::glib::ffi::g_source_remove(id) };
    }
}

/// Asks for the plugin `id` to be reloaded after the current callback returns.
pub fn request_reload(id: &str) {
    let id = id.to_owned();
    glib::idle_add_local_once(move || {
        if let Err(err) = reload(Some(&id)) {
            eprintln!("rsimgv: plugin {id}: reload failed: {err}");
        }
    });
}
