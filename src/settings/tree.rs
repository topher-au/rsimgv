//! The application's settings by dotted name. GSettings key names cannot hold a `.`, so a group of settings is a child
//! schema, and `window.sidebar.width` is the key `width` of the child at `window.sidebar`. `Settings` holds a
//! `gio::Settings` for every group, and gives each call to the one the name is in.

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

    pub fn disconnect(&self, handler: ChangedHandler) {
        handler.group.disconnect(handler.id);
    }

    /// An action called `action_name` that holds the boolean setting `key` as its state, and toggles it when activated.
    /// `gio::Settings::create_action` would name the action after the last part of the key.
    pub fn create_toggle_action(&self, key: &str, action_name: &str) -> gio::SimpleAction {
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
                action.set_state(&on.to_variant());
            }
        });
        let weak = action.downgrade();
        self.connect_changed(key, move |settings, key| {
            if let Some(action) = weak.upgrade() {
                action.set_state(&settings.boolean(key).to_variant());
            }
        });
        action
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
