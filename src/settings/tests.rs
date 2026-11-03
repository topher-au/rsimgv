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

use super::YamlSettingsBackend;

/// A schema at `/test/rsimgv/`, and a relocatable schema for settings nested below it.
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
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The YAML document saved in `file`.
fn saved(file: &Path) -> Value {
    serde_yaml_ng::from_str(&fs::read_to_string(file).expect("the settings file exists")).expect("the file is YAML")
}

#[test]
fn defaults_apply_when_nothing_is_saved() {
    let fixture = Fixture::new("defaults");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
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
        let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
        let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
        settings.set_int("window-width", 1200).unwrap();
    }
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 1200);
}

#[test]
fn every_value_keeps_its_type() {
    let fixture = Fixture::new("types");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_boolean("show-hidden", true).unwrap();
    settings.set_string("title", "a \"quoted\"\nline with ünïcode").unwrap();
    settings.set_strv("recent-files", ["/home/user/notes.txt", "it's"]).unwrap();

    let reopened = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = reopened.settings(&fixture.schema("test.rsimgv"), None);
    assert!(settings.boolean("show-hidden"));
    assert_eq!(settings.string("title"), "a \"quoted\"\nline with ünïcode");
    assert_eq!(settings.strv("recent-files"), ["/home/user/notes.txt", "it's"][..]);
}

#[test]
fn a_reset_returns_the_schema_default() {
    let fixture = Fixture::new("reset");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_int("window-width", 1200).unwrap();
    settings.reset("window-width");
    assert_eq!(settings.int("window-width"), 960);
    assert!(!fs::read_to_string(fixture.file()).unwrap().contains("window-width"), "the reset value is removed from the file");
}

#[test]
fn nested_paths_nest_in_the_file() {
    let fixture = Fixture::new("nested");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv.nested"), Some("/test/rsimgv/group/parent/"));
    settings.set_int("child", 8).unwrap();

    let root = saved(&fixture.file());
    assert_eq!(root["test"]["rsimgv"]["group"]["parent"]["child"]["value"], Value::String("8".into()));
    assert_eq!(root["test"]["rsimgv"]["group"]["parent"]["child"]["type"], Value::String("i".into()));

    let reopened = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = reopened.settings(&fixture.schema("test.rsimgv.nested"), Some("/test/rsimgv/group/parent/"));
    assert_eq!(settings.int("child"), 8);
}

#[test]
fn a_delayed_apply_saves_the_batch_once_and_a_revert_discards_it() {
    let fixture = Fixture::new("delay");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);

    settings.delay();
    settings.set_int("window-width", 1200).unwrap();
    settings.set_string("title", "changed").unwrap();
    assert!(!fixture.file().exists(), "nothing is saved until the batch is applied");
    settings.apply();

    let reopened = YamlSettingsBackend::new(fixture.file()).unwrap();
    let saved_settings = reopened.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(saved_settings.int("window-width"), 1200);
    assert_eq!(saved_settings.string("title"), "changed");

    settings.delay();
    settings.set_int("window-width", 5).unwrap();
    settings.revert();
    assert_eq!(settings.int("window-width"), 1200);
}

#[test]
fn a_write_notifies_other_settings_objects_but_not_its_writer() {
    let fixture = Fixture::new("notify");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
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
    assert!(written.borrow().is_empty(), "the writer is not told about its own change");
}

#[test]
fn a_batch_notifies_each_key_it_changed() {
    let fixture = Fixture::new("batch-notify");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
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
}

#[test]
fn two_backends_on_one_file_keep_each_others_writes() {
    let fixture = Fixture::new("two-writers");
    let first = YamlSettingsBackend::new(fixture.file()).unwrap();
    let second = YamlSettingsBackend::new(fixture.file()).unwrap();
    let schema = fixture.schema("test.rsimgv");
    first.settings(&schema, None).set_int("window-width", 1).unwrap();
    second.settings(&schema, None).set_string("title", "second").unwrap();

    let reopened = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = reopened.settings(&schema, None);
    assert_eq!(settings.int("window-width"), 1, "the first write is kept");
    assert_eq!(settings.string("title"), "second", "the second write is kept");
}

#[test]
fn an_outside_edit_is_read_and_notified() {
    let fixture = Fixture::new("outside-edit");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    let changed = Rc::new(RefCell::new(false));
    let sink = changed.clone();
    settings.connect_changed(Some("window-width"), move |_, _| *sink.borrow_mut() = true);

    fs::write(
        fixture.file(),
        "test:\n  rsimgv:\n    window-width:\n      type: i\n      value: '1500'\n",
    )
    .unwrap();
    pump_until(|| *changed.borrow());
    assert!(*changed.borrow(), "the edit is announced");
    assert_eq!(settings.int("window-width"), 1500);
}

#[test]
fn a_malformed_file_is_read_only_and_left_as_it_is() {
    let fixture = Fixture::new("malformed");
    fs::write(fixture.file(), "window: [").unwrap();
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
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
        "test:\n  rsimgv:\n    window-width:\n      type: s\n      value: \"'wide'\"\n",
    )
    .unwrap();
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    assert_eq!(settings.int("window-width"), 960);
}

#[test]
fn a_missing_parent_directory_is_created_on_the_first_save() {
    let fixture = Fixture::new("parent");
    let file = fixture.dir.join("nested").join("settings.yaml");
    let backend = YamlSettingsBackend::new(&file).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_int("window-width", 1).unwrap();
    assert!(file.exists());
}

#[test]
fn a_missing_file_leaves_no_settings() {
    let fixture = Fixture::new("missing");
    let backend = YamlSettingsBackend::new(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    settings.set_int("window-width", 1200).unwrap();
    fs::remove_file(fixture.file()).unwrap();
    let settings = backend.settings(&fixture.schema("test.rsimgv"), None);
    pump_until(|| settings.int("window-width") == 960);
    assert_eq!(settings.int("window-width"), 960, "a deleted file falls back to the defaults");
}
