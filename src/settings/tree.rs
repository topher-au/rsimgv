//! The application's settings by dotted name. GSettings key names cannot hold a `.`, so a group of settings is a child
//! schema, and `window.sidebar.width` is the key `width` of the child at `window.sidebar`. `Settings` holds a
//! `gio::Settings` for every group, and gives each call to the one the name is in.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gio::prelude::*;
use glib::collections::strv::IntoStrV;
use glib::variant::{FromVariant, ToVariant};
use gtk::{gio, glib};

/// Settings by dotted name. Clones share the same `gio::Settings` objects. A name that is not in the schema panics, as
/// it does with `gio::Settings`.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The settings of each group by its dotted path, with the empty path for the root.
    groups: Rc<HashMap<String, gio::Settings>>,
}

/// A connection made by `Settings::connect_changed`, which `Settings::disconnect` ends.
#[derive(Debug)]
pub struct ChangedHandler {
    group: gio::Settings,
    id: glib::SignalHandlerId,
}

impl Settings {
    /// The settings of `root` and of all the child schemas below it.
    pub fn new(root: gio::Settings) -> Self {
        let mut groups = HashMap::new();
        collect(String::new(), root, &mut groups);
        Self { groups: Rc::new(groups) }
    }

    /// The group that `key` is in, and its name in that group.
    fn locate<'a>(&self, key: &'a str) -> (&gio::Settings, &'a str) {
        let (group, name) = key.rsplit_once('.').unwrap_or(("", key));
        let settings = self.groups.get(group).unwrap_or_else(|| panic!("the settings have no group for the key {key}"));
        (settings, name)
    }

    /// Whether the schema has the group `group`, the empty name being the root. It is used to check an installed
    /// schema against the one the build expects, so an older one is reported before a setting in it is read.
    pub fn has_group(&self, group: &str) -> bool {
        self.groups.contains_key(group)
    }

    pub fn boolean(&self, key: &str) -> bool {
        let (group, name) = self.locate(key);
        group.boolean(name)
    }

    pub fn int(&self, key: &str) -> i32 {
        let (group, name) = self.locate(key);
        group.int(name)
    }

    pub fn double(&self, key: &str) -> f64 {
        let (group, name) = self.locate(key);
        group.double(name)
    }

    pub fn string(&self, key: &str) -> glib::GString {
        let (group, name) = self.locate(key);
        group.string(name)
    }

    pub fn strv(&self, key: &str) -> glib::StrV {
        let (group, name) = self.locate(key);
        group.strv(name)
    }

    pub fn value(&self, key: &str) -> glib::Variant {
        let (group, name) = self.locate(key);
        group.value(name)
    }

    /// Whether the schema has a setting called `key`.
    pub fn has(&self, key: &str) -> bool {
        self.lookup(key).is_some()
    }

    /// The group that `key` is in and its name in that group, if the schema has it.
    fn lookup<'a>(&'a self, key: &'a str) -> Option<(&'a gio::Settings, &'a str)> {
        let (group, name) = key.rsplit_once('.').unwrap_or(("", key));
        let settings = self.groups.get(group)?;
        let schema = settings.settings_schema()?;
        schema.has_key(name).then_some((settings, name))
    }

    /// The schema entry for `key`, if the schema has it.
    fn schema_key(&self, key: &str) -> Option<gio::SettingsSchemaKey> {
        let (settings, name) = self.lookup(key)?;
        settings.settings_schema().map(|schema| schema.key(name))
    }

    /// The dotted name of every setting in the schema, sorted.
    pub fn keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for (group, settings) in &*self.groups {
            let Some(schema) = settings.settings_schema() else { continue };
            for name in schema.list_keys() {
                keys.push(if group.is_empty() { name.to_string() } else { format!("{group}.{name}") });
            }
        }
        keys.sort();
        keys
    }

    /// The GVariant type of `key`, if the schema has it.
    pub fn value_type(&self, key: &str) -> Option<glib::VariantType> {
        self.schema_key(key).map(|key| key.value_type())
    }

    /// The default value of `key`, if the schema has it.
    pub fn default_value(&self, key: &str) -> Option<glib::Variant> {
        self.schema_key(key).map(|key| key.default_value())
    }

    /// The description of `key`, if the schema has it.
    pub fn description(&self, key: &str) -> Option<glib::GString> {
        self.schema_key(key).and_then(|key| key.description())
    }

    /// The summary of `key`, if the schema has it.
    pub fn summary(&self, key: &str) -> Option<glib::GString> {
        self.schema_key(key).and_then(|key| key.summary())
    }

    /// Whether `value` fits the range of `key`. A key that the schema does not have fits nothing.
    pub fn range_check(&self, key: &str, value: &glib::Variant) -> bool {
        self.schema_key(key).is_some_and(|key| key.range_check(value))
    }

    /// Resets `key` to its schema default. Returns false if the schema has no such key.
    pub fn reset_key(&self, key: &str) -> bool {
        let Some((group, name)) = self.lookup(key) else { return false };
        group.reset(name);
        true
    }

    pub fn get<T: FromVariant>(&self, key: &str) -> T {
        let (group, name) = self.locate(key);
        group.get(name)
    }

    pub fn set_boolean(&self, key: &str, value: bool) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_boolean(name, value)
    }

    pub fn set_int(&self, key: &str, value: i32) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_int(name, value)
    }

    pub fn set_double(&self, key: &str, value: f64) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_double(name, value)
    }

    pub fn set_string(&self, key: &str, value: &str) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_string(name, value)
    }

    pub fn set_strv(&self, key: &str, value: impl IntoStrV) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_strv(name, value)
    }

    pub fn set_value(&self, key: &str, value: &glib::Variant) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set_value(name, value)
    }

    pub fn set<T: Into<glib::Variant>>(&self, key: &str, value: T) -> Result<(), glib::BoolError> {
        let (group, name) = self.locate(key);
        group.set(name, value)
    }

    pub fn reset(&self, key: &str) {
        let (group, name) = self.locate(key);
        group.reset(name);
    }

    pub fn bind<'a, P: IsA<glib::Object>>(&'a self, key: &'a str, object: &'a P, property: &'a str) -> gio::BindingBuilder<'a> {
        let (group, name) = self.locate(key);
        group.bind(name, object, property)
    }

    /// Calls `callback` with these settings and the dotted name whenever `key` changes. The callback does not keep the
    /// settings alive.
    pub fn connect_changed<F: Fn(&Settings, &str) + 'static>(&self, key: &str, callback: F) -> ChangedHandler {
        let (group, name) = self.locate(key);
        let groups = Rc::downgrade(&self.groups);
        let dotted = key.to_owned();
        let id = group.connect_changed(Some(name), move |_, _| {
            if let Some(groups) = groups.upgrade() {
                callback(&Settings { groups }, &dotted);
            }
        });
        ChangedHandler { group: group.clone(), id }
    }

    /// Runs `update` with these settings whenever `key` changes, while `guard` is alive. Once it is gone the handler
    /// disconnects itself, so a torn-down window is never touched through its old connections. Use this for updates
    /// that poke objects which die with the window, such as actions. Disconnect the returned handle when the window
    /// is destroyed, so no later change can reach its objects while they are being torn down.
    pub fn connect_guarded<T, F>(&self, key: &str, guard: glib::WeakRef<T>, update: F) -> GuardHandle
    where
        T: glib::object::ObjectType,
        F: Fn(&Settings) + 'static,
    {
        let (group, name) = self.locate(key);
        let group: gio::Settings = group.clone();
        let settings = self.clone();
        // The handler's own id, shared with the returned handle: whoever takes it first disconnects, and the other
        // sees nothing left to do.
        let id: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
        let own = id.clone();
        let watched = group.clone();
        *id.borrow_mut() = Some(group.connect_changed(Some(name), move |_, _| {
            if own.borrow().is_none() || guard.upgrade().is_none() {
                if let Some(id) = own.borrow_mut().take() {
                    watched.disconnect(id);
                }
                return;
            }
            update(&settings);
        }));
        GuardHandle { group: group.clone(), id }
    }

    pub fn disconnect(&self, handler: ChangedHandler) {
        handler.group.disconnect(handler.id);
    }

    /// An action called `action_name` that holds the boolean setting `key` as its state, and toggles it when activated.
    /// `gio::Settings::create_action` would name the action after the last part of the key.
    ///
    /// The returned handle ends the settings connection: disconnect it when the window is destroyed, so no later
    /// change can reach the action while it is being torn down. Once the action is gone the handler disconnects
    /// itself.
    pub fn create_toggle_action(&self, key: &str, action_name: &str) -> (gio::SimpleAction, GuardHandle) {
        let action = gio::SimpleAction::new_stateful(action_name, None, &self.boolean(key).to_variant());
        action.connect_change_state({
            let settings = self.clone();
            let key = key.to_owned();
            move |action, value| {
                let Some(on) = value.and_then(|value| value.get::<bool>()) else { return };
                if let Err(err) = settings.set_boolean(&key, on) {
                    eprintln!("rsimgv: could not save {key}: {err}");
                    return;
                }
                // The write above already brought the action up to date through the handler below; this is only for
                // a backend that did not announce it.
                sync_state(action, on);
            }
        });
        let key = key.to_owned();
        let live = action.downgrade();
        let weak = live.clone();
        let name = key.clone();
        let guard = self.connect_guarded(&name, live, move |settings| {
            if let Some(action) = weak.upgrade() {
                sync_state(&action, settings.boolean(&key));
            }
        });
        (action, guard)
    }
}

/// Ends the settings connection of a guarded update, such as a toggle action's. Disconnecting twice does nothing:
/// the handler and the handle share the connection, and whoever takes it first disconnects.
pub struct GuardHandle {
    group: gio::Settings,
    id: Rc<RefCell<Option<glib::SignalHandlerId>>>,
}

impl GuardHandle {
    pub fn disconnect(&self) {
        if let Some(id) = self.id.borrow_mut().take() {
            self.group.disconnect(id);
        }
    }
}

/// Brings `action` to `on`, unless it is there already. Setting the state notifies the action's group, so a
/// redundant write would poke a window that is being torn down for no reason.
fn sync_state(action: &gio::SimpleAction, on: bool) {
    let state = on.to_variant();
    if action.state().as_ref() != Some(&state) {
        action.set_state(&state);
    }
}

/// Adds `settings` to `groups` at `path`, with all the child schemas below it.
fn collect(path: String, settings: gio::Settings, groups: &mut HashMap<String, gio::Settings>) {
    for name in settings.list_children() {
        let child_path = if path.is_empty() { name.to_string() } else { format!("{path}.{name}") };
        collect(child_path, settings.child(&name), groups);
    }
    groups.insert(path, settings);
}
