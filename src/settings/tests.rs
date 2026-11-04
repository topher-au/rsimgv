//! Tests of the settings backend through real `gio::Settings` objects. Each test compiles its own schema with
//! `glib-compile-schemas`, and works in its own directory.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::prelude::*;
use gtk::{gio, glib};
use serde_yaml_ng::Value;

use super::{Settings, YamlSettingsBackend};

/// The path of the test schema, which the top level of the settings file is at.
const ROOT: &str = "/test/rsimgv/";

/// A schema at `/test/rsimgv/`, a relocatable schema for settings nested below it, and child schemas for dotted names.
const SCHEMA_XML: &str = r#"<schemalist>
  <schema id="test.rsimgv" path="/test/rsimgv/">
    <key name="window-width" type="i"><default>960</default></key>
    <key name="show-hidden" type="b"><default>false</default></key>
    <key name="title" type="s"><default>"untitled"</default></key>
    <key name="recent-files" type="as"><default>[]</default></key>
  </schema>
  <schema id="test.rsimgv.nested">
    <key name="child" type="i"><default>7</default></key>
  </schema>
  <schema id="test.tree" path="/test/rsimgv/">
    <child name="window" schema="test.tree.window"/>
  </schema>
  <schema id="test.tree.window" path="/test/rsimgv/window/">
    <child name="sidebar" schema="test.tree.window.sidebar"/>
    <key name="height" type="i"><default>640</default></key>
    <key name="maximized" type="b"><default>false</default></key>
  </schema>
  <schema id="test.tree.window.sidebar" path="/test/rsimgv/window/sidebar/">
    <key name="width" type="i"><default>-1</default></key>
  </schema>
</schemalist>"#;

/// A directory holding the compiled test schema, removed when the test ends.
struct Fixture {
    dir: PathBuf,
    source: gio::SettingsSchemaSource,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rsimgv-settings-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the fixture directory");
        fs::write(dir.join("test.rsimgv.gschema.xml"), SCHEMA_XML).expect("write the schema");
        let status = Command::new("glib-compile-schemas").arg(&dir).status().expect("glib-compile-schemas runs");
        assert!(status.success(), "the test schema compiles");
        let source = gio::SettingsSchemaSource::from_directory(&dir, None, true).expect("load the test schema");
        Self { dir, source }
    }

    fn schema(&self, id: &str) -> gio::SettingsSchema {
        self.source.lookup(id, false).expect("the test schema is compiled")
    }

    /// The settings file of the test, which does not exist until something is saved.
    fn file(&self) -> PathBuf {
        self.dir.join("settings.yaml")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Runs the main context until `done` holds, or for five seconds.
fn pump_until(mut done: impl FnMut() -> bool) {
    let context = glib::MainContext::thread_default().unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `test` with a main context of its own. The whole test binary shares one process, and the tests of other
/// modules leave their async work on the default context; a test that pumps it would poll that work on its own thread,
/// which GLib refuses. A context of its own keeps the test to its own signals and file monitor.
fn in_context<T>(test: impl FnOnce() -> T) -> T {
    glib::MainContext::new().with_thread_default(test).expect("a fresh main context is not owned by another thread")
}

/// The YAML document saved in `file`.
fn saved(file: &Path) -> Value {
    serde_yaml_ng::from_str(&fs::read_to_string(file).expect("the settings file exists")).expect("the file is YAML")
}

#[test]
fn defaults_apply_when_nothing_is_saved() {
    let fixture = Fixture::new("defaults");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 960);
    assert!(!settings.boolean("show-hidden"));
    assert_eq!(settings.string("title"), "untitled");
    assert!(settings.strv("recent-files").is_empty());
    assert!(!fixture.file().exists(), "reading saves nothing");
}

#[test]
fn a_value_persists_across_backends() {
    let fixture = Fixture::new("persist");
    {
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
        settings.set_int("window-width", 1200).unwrap();
    }
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 1200);
}

#[test]
fn every_value_keeps_its_type() {
    let fixture = Fixture::new("types");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_boolean("show-hidden", true).unwrap();
    settings.set_string("title", "a \"quoted\"\nline with ünïcode").unwrap();
    settings.set_strv("recent-files", ["/home/user/notes.txt", "it's"]).unwrap();

    let reopened = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = reopened.settings(&fixture.schema("test.rsimgv"), None);
    assert!(settings.boolean("show-hidden"));
    assert_eq!(settings.string("title"), "a \"quoted\"\nline with ünïcode");
    assert_eq!(settings.strv("recent-files"), ["/home/user/notes.txt", "it's"][..]);
}

#[test]
fn a_reset_returns_the_schema_default() {
    let fixture = Fixture::new("reset");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_int("window-width", 1200).unwrap();
    settings.reset("window-width");
    assert_eq!(settings.int("window-width"), 960);
    assert!(!fs::read_to_string(fixture.file()).unwrap().contains("window-width"), "the reset value is removed from the file");
}

#[test]
fn nested_paths_nest_in_the_file() {
    let fixture = Fixture::new("nested");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv.nested"), Some("/test/rsimgv/group/parent/"));
    settings.set_int("child", 8).unwrap();

    let root = saved(&fixture.file());
    assert_eq!(root["group"]["parent"]["child"], Value::Number(8.into()));

    let reopened = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = reopened.settings(&fixture.schema("test.rsimgv.nested"), Some("/test/rsimgv/group/parent/"));
    assert_eq!(settings.int("child"), 8);
}

#[test]
fn a_delayed_apply_saves_the_batch_once_and_a_revert_discards_it() {
    let fixture = Fixture::new("delay");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);

    settings.delay();
    settings.set_int("window-width", 1200).unwrap();
    settings.set_string("title", "changed").unwrap();
    assert!(!fixture.file().exists(), "nothing is saved until the batch is applied");
    settings.apply();

    let reopened = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let saved_settings = reopened.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(saved_settings.int("window-width"), 1200);
    assert_eq!(saved_settings.string("title"), "changed");

    settings.delay();
    settings.set_int("window-width", 5).unwrap();
    settings.revert();
    assert_eq!(settings.int("window-width"), 1200);
}

#[test]
fn a_write_notifies_other_settings_objects() {
    in_context(|| {
        let fixture = Fixture::new("notify");
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let schema = fixture.schema("test.rsimgv");
        let writer = backend.settings(&schema, None);
        let observer = backend.settings(&schema, None);
        let observed = Rc::new(RefCell::new(Vec::new()));
        let written = Rc::new(RefCell::new(Vec::new()));
        let sink = observed.clone();
        observer.connect_changed(Some("window-width"), move |_, key| sink.borrow_mut().push(key.to_owned()));
        let sink = written.clone();
        writer.connect_changed(Some("window-width"), move |_, key| sink.borrow_mut().push(key.to_owned()));

        writer.set_int("window-width", 1200).unwrap();
        pump_until(|| !observed.borrow().is_empty());
        assert_eq!(observed.borrow().as_slice(), ["window-width"]);
        // GLib passes no origin tag for a direct write, so the writer hears about its own change too. It must hear
        // about nothing else.
        assert!(written.borrow().iter().all(|key| key == "window-width"));
    });
}

#[test]
fn a_batch_notifies_each_key_it_changed() {
    in_context(|| {
        let fixture = Fixture::new("batch-notify");
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let schema = fixture.schema("test.rsimgv");
        let writer = backend.settings(&schema, None);
        let observer = backend.settings(&schema, None);
        let keys = Rc::new(RefCell::new(Vec::new()));
        let sink = keys.clone();
        observer.connect_changed(None, move |_, key| sink.borrow_mut().push(key.to_owned()));

        writer.delay();
        writer.set_int("window-width", 1200).unwrap();
        writer.set_boolean("show-hidden", true).unwrap();
        writer.apply();
        pump_until(|| keys.borrow().len() >= 2);
        let mut seen = keys.borrow().clone();
        seen.sort();
        assert_eq!(seen, ["show-hidden", "window-width"]);
    });
}

#[test]
fn two_backends_on_one_file_keep_each_others_writes() {
    let fixture = Fixture::new("two-writers");
    let first = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let second = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let schema = fixture.schema("test.rsimgv");
    first.settings(&schema, None).set_int("window-width", 1).unwrap();
    second.settings(&schema, None).set_string("title", "second").unwrap();

    let reopened = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = reopened.settings(&schema, None);
    assert_eq!(settings.int("window-width"), 1, "the first write is kept");
    assert_eq!(settings.string("title"), "second", "the second write is kept");
}

#[test]
fn an_outside_edit_is_read_and_notified() {
    in_context(|| {
        let fixture = Fixture::new("outside-edit");
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
        let changed = Rc::new(RefCell::new(false));
        let sink = changed.clone();
        settings.connect_changed(Some("window-width"), move |_, _| *sink.borrow_mut() = true);

        fs::write(
            fixture.file(),
            "window-width: 1500\n",
        )
        .unwrap();
        pump_until(|| *changed.borrow());
        assert!(*changed.borrow(), "the edit is announced");
        assert_eq!(settings.int("window-width"), 1500);
    });
}

#[test]
fn a_malformed_file_is_read_only_and_left_as_it_is() {
    let fixture = Fixture::new("malformed");
    fs::write(fixture.file(), "window: [").unwrap();
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert!(!settings.is_writable("window-width"));
    assert!(settings.set_int("window-width", 1).is_err(), "the write is refused");
    assert_eq!(fs::read_to_string(fixture.file()).unwrap(), "window: [");
}

#[test]
fn a_value_of_another_type_in_the_file_reads_as_the_default() {
    let fixture = Fixture::new("wrong-type");
    fs::write(
        fixture.file(),
        "window-width: wide\n",
    )
    .unwrap();
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 960);
}

#[test]
fn a_value_that_does_not_fit_stays_until_it_is_replaced() {
    let fixture = Fixture::new("kept");
    fs::write(fixture.file(), "window-width: wide\ntitle: kept\n").unwrap();
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_boolean("show-hidden", true).unwrap();
    assert_eq!(saved(&fixture.file())["window-width"], Value::String("wide".into()), "another write leaves it");

    settings.set_int("window-width", 1200).unwrap();
    assert_eq!(saved(&fixture.file())["window-width"], Value::Number(1200.into()));
    assert_eq!(settings.string("title"), "kept");
}

#[test]
fn a_value_replaces_a_group_under_its_name() {
    let fixture = Fixture::new("group-replaced");
    fs::write(fixture.file(), "window-width:\n  type: i\n  value: '5'\n").unwrap();
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 960, "a group is not the setting");

    settings.set_int("window-width", 1200).unwrap();
    assert_eq!(saved(&fixture.file())["window-width"], Value::Number(1200.into()));
    assert_eq!(settings.int("window-width"), 1200);
}

#[test]
fn a_missing_parent_directory_is_created_on_the_first_save() {
    let fixture = Fixture::new("parent");
    let file = fixture.dir.join("nested").join("settings.yaml");
    let backend = YamlSettingsBackend::new(&file, ROOT).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_int("window-width", 1).unwrap();
    assert!(file.exists());
}

#[test]
fn a_missing_file_leaves_no_settings() {
    in_context(|| {
        let fixture = Fixture::new("missing");
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
        settings.set_int("window-width", 1200).unwrap();
        fs::remove_file(fixture.file()).unwrap();
        let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
        pump_until(|| settings.int("window-width") == 960);
        assert_eq!(settings.int("window-width"), 960, "a deleted file falls back to the defaults");
    });
}

#[test]
fn a_dotted_name_reads_and_writes_the_nested_setting() {
    let fixture = Fixture::new("dotted");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let settings = Settings::new(backend.settings(&fixture.schema("test.tree"), None));
    assert_eq!(settings.int("window.height"), 640);
    assert_eq!(settings.int("window.sidebar.width"), -1);

    settings.set_int("window.height", 800).unwrap();
    settings.set_int("window.sidebar.width", 200).unwrap();
    assert_eq!(settings.int("window.height"), 800);

    let root = saved(&fixture.file());
    assert_eq!(root["window"]["height"], Value::Number(800.into()));
    assert_eq!(root["window"]["sidebar"]["width"], Value::Number(200.into()));

    settings.reset("window.height");
    assert_eq!(settings.int("window.height"), 640);
}

#[test]
fn a_change_is_reported_by_its_dotted_name() {
    in_context(|| {
        let fixture = Fixture::new("dotted-notify");
        let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
        let schema = fixture.schema("test.tree");
        let writer = Settings::new(backend.settings(&schema, None));
        let observer = Settings::new(backend.settings(&schema, None));
        let keys = Rc::new(RefCell::new(Vec::new()));
        let sink = keys.clone();
        let handler = observer.connect_changed("window.sidebar.width", move |settings, key| {
            sink.borrow_mut().push((key.to_owned(), settings.int(key)));
        });

        writer.set_int("window.sidebar.width", 300).unwrap();
        pump_until(|| !keys.borrow().is_empty());
        assert_eq!(keys.borrow().as_slice(), [("window.sidebar.width".to_owned(), 300)]);

        observer.disconnect(handler);
        writer.set_int("window.sidebar.width", 400).unwrap();
        pump_until(|| observer.int("window.sidebar.width") == 400);
        assert_eq!(keys.borrow().len(), 1, "a disconnected handler is not called");
    });
}

#[test]
fn a_toggle_action_follows_its_setting_under_its_own_name() {
    let fixture = Fixture::new("toggle-action");
    let backend = YamlSettingsBackend::new(fixture.file(), ROOT).unwrap();
    let schema = fixture.schema("test.tree");
    let settings = Settings::new(backend.settings(&schema, None));
    let action = settings.create_toggle_action("window.maximized", "maximize-window");
    assert_eq!(action.name(), "maximize-window");

    action.activate(None);
    assert!(settings.boolean("window.maximized"), "activating toggles the setting");
    assert_eq!(action.state().and_then(|state| state.get::<bool>()), Some(true));

    let other = Settings::new(backend.settings(&schema, None));
    other.set_boolean("window.maximized", false).unwrap();
    pump_until(|| action.state().and_then(|state| state.get::<bool>()) == Some(false));
    assert_eq!(action.state().and_then(|state| state.get::<bool>()), Some(false), "the state follows the setting");
}
