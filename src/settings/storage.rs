//! The settings file: one YAML document nested by the segments of each setting's GSettings key path, to any depth. A
//! setting is a mapping holding its exact GVariant `type` and its GVariant text as `value`, so no YAML scalar is ever
//! read as a setting's type:
//!
//! ```yaml
//! au:
//!   topher:
//!     rsimgv:
//!       window-width:
//!         type: i
//!         value: "960"
//!       recent-files:
//!         type: as
//!         value: "['/home/user/notes.txt']"
//! ```
//!
//! A mapping with `type` and `value` keys is always a setting, so those two names cannot be used for a group. An entry
//! that does not parse is kept as it was written, so a save never discards it, and it reads as absent. A file in the
//! layout that came before this one is read by `legacy`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use gtk::glib;
use serde_yaml_ng::{Mapping, Value};

use super::legacy::{self, Legacy};

/// A key of a schema, as the file needs it: its full key path, its name within the schema, and its GVariant type.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaKey {
    pub path: String,
    pub name: String,
    pub ty: String,
}

/// The settings in a file.
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// The settings that parse, by full key path.
    pub values: BTreeMap<String, glib::Variant>,
    /// The entries that do not parse, by full key path, as they were written.
    pub invalid: BTreeMap<String, Value>,
}

/// Reads the document in `text`. Settings that the layout before this one saved under `keys` are migrated to typed
/// values.
pub fn parse(text: &str, keys: &[SchemaKey]) -> anyhow::Result<Document> {
    let root = match serde_yaml_ng::from_str::<Value>(text).context("the settings are not valid YAML")? {
        Value::Null => Mapping::new(),
        Value::Mapping(root) => root,
        _ => bail!("the top level of the settings is not a mapping"),
    };

    let mut document = Document::default();
    let mut legacy = Legacy::new();
    walk(&root, &mut Vec::new(), &mut document, &mut legacy);
    for (path, variant) in legacy::migrate(&legacy, keys) {
        // A setting saved in the typed format wins over the same setting in the layout before it.
        document.values.entry(path).or_insert(variant);
    }
    Ok(document)
}

/// Reads the mapping `node`, whose path so far is `segments`, into `document`. A mapping that is not a setting is a
/// group. A scalar is a setting saved in the layout before this one, which `legacy` keeps under its group and name.
fn walk(node: &Mapping, segments: &mut Vec<String>, document: &mut Document, legacy: &mut Legacy) {
    for (key, value) in node {
        let Some(name) = key.as_str() else {
            eprintln!("rsimgv: skipping a settings entry whose key is not a string");
            continue;
        };
        match value {
            Value::Mapping(inner) if is_entry(inner) => {
                let path = full_path(segments, name);
                match entry(value) {
                    Some(variant) => {
                        document.values.insert(path, variant);
                    }
                    None => {
                        eprintln!("rsimgv: the setting {path} is not a typed value; it is kept as written and ignored");
                        document.invalid.insert(path, value.clone());
                    }
                }
            }
            Value::Mapping(inner) => {
                segments.push(name.to_owned());
                walk(inner, segments, document, legacy);
                segments.pop();
            }
            _ => match segments.split_first() {
                Some((group, rest)) => {
                    let name = rest.iter().map(String::as_str).chain([name]).collect::<Vec<_>>().join(".");
                    legacy.insert((group.clone(), name), value.clone());
                }
                None => {
                    document.invalid.insert(format!("/{name}"), value.clone());
                }
            },
        }
    }
}

/// The full key path of the setting `name` under `segments`.
fn full_path(segments: &[String], name: &str) -> String {
    let mut path = String::new();
    for segment in segments.iter().map(String::as_str).chain([name]) {
        path.push('/');
        path.push_str(segment);
    }
    path
}

/// Whether `mapping` is a setting rather than a group: it has a `type` and a `value`.
fn is_entry(mapping: &Mapping) -> bool {
    mapping.get("type").is_some() && mapping.get("value").is_some()
}

/// The GVariant of a setting, if it has a valid `type` and a `value` of that type. Only text is accepted as the value,
/// so a YAML number or boolean is never mistaken for a setting's type.
fn entry(value: &Value) -> Option<glib::Variant> {
    let mapping = value.as_mapping()?;
    let ty = glib::VariantTy::new(mapping.get("type")?.as_str()?).ok()?;
    let text = mapping.get("value")?.as_str()?;
    let variant = glib::Variant::parse(Some(ty), text).ok()?;
    (variant.type_() == ty).then_some(variant)
}

/// The YAML text of `document`.
pub fn serialize(document: &Document) -> anyhow::Result<String> {
    let mut root = Mapping::new();
    for (path, variant) in &document.values {
        let mut entry = Mapping::new();
        entry.insert(Value::String("type".to_owned()), Value::String(variant.type_().as_str().to_owned()));
        entry.insert(Value::String("value".to_owned()), Value::String(variant.print(false).to_string()));
        insert_at(&mut root, path, Value::Mapping(entry));
    }
    for (path, value) in &document.invalid {
        insert_at(&mut root, path, value.clone());
    }
    serde_yaml_ng::to_string(&Value::Mapping(root)).context("the settings could not be written as YAML")
}

/// Puts `value` in `node` at the key path `path`, in a nested mapping for each segment before its last.
fn insert_at(node: &mut Mapping, path: &str, value: Value) {
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    insert_segments(node, &segments, value);
}

fn insert_segments(node: &mut Mapping, segments: &[&str], value: Value) {
    match segments {
        [] => {}
        [last] => {
            node.insert(Value::String((*last).to_owned()), value);
        }
        [first, rest @ ..] => {
            let key = Value::String((*first).to_owned());
            if !matches!(node.get(&key), Some(Value::Mapping(_))) {
                node.insert(key.clone(), Value::Mapping(Mapping::new()));
            }
            if let Some(Value::Mapping(inner)) = node.get_mut(&key) {
                insert_segments(inner, rest, value);
            }
        }
    }
}

/// The document in `file`. A missing file holds no settings.
pub fn load(file: &Path, keys: &[SchemaKey]) -> anyhow::Result<Document> {
    match std::fs::read_to_string(file) {
        Ok(text) => parse(&text, keys).with_context(|| format!("could not read {}", file.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Document::default()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Writes `document` to `file` as one atomic replacement, creating the parent directory if it is missing.
pub fn save(file: &Path, document: &Document) -> anyhow::Result<()> {
    let text = serialize(document)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
    }
    crate::storage::atomic_file::write(file, text.as_bytes())
        .with_context(|| format!("could not write {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::ToVariant;

    /// One value of each type the schemas use, as GVariant text, with the tricky strings that need escaping.
    const SAMPLES: &[(&str, &str)] = &[
        ("b", "true"),
        ("y", "255"),
        ("n", "-300"),
        ("q", "65000"),
        ("i", "-2147483648"),
        ("u", "4294967295"),
        ("x", "-9007199254740993"),
        ("t", "18446744073709551615"),
        ("d", "1.5"),
        ("s", "'quote \" and \\\\ backslash\\nnewline ünïcödé'"),
        ("o", "'/org/example/object'"),
        ("g", "'a{sv}'"),
        ("as", "['size', 'modified']"),
        ("ai", "[1, -2, 3]"),
        ("(is)", "(7, 'seven')"),
        ("a{sv}", "{'speed': <1.25>, 'name': <'x'>}"),
        ("mi", "just 3"),
        ("ms", "nothing"),
    ];

    #[test]
    fn every_supported_type_round_trips() {
        let mut document = Document::default();
        for (index, (ty, text)) in SAMPLES.iter().enumerate() {
            let variant = glib::Variant::parse(Some(glib::VariantTy::new(ty).unwrap()), text).unwrap();
            document.values.insert(format!("/test/key-{index}"), variant);
        }
        let text = serialize(&document).unwrap();
        let parsed = parse(&text, &[]).unwrap();
        assert_eq!(parsed.values, document.values);
        assert!(parsed.invalid.is_empty());
    }

    #[test]
    fn settings_nest_to_any_depth() {
        let mut document = Document::default();
        document.values.insert("/au/topher/rsimgv/window-width".to_owned(), 1200i32.to_variant());
        document.values.insert("/a/b/c/d/e/deep".to_owned(), true.to_variant());
        document.values.insert("/top".to_owned(), "x".to_variant());
        let text = serialize(&document).unwrap();
        let root: Value = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(root["au"]["topher"]["rsimgv"]["window-width"]["value"], Value::String("1200".into()));
        assert_eq!(root["a"]["b"]["c"]["d"]["e"]["deep"]["type"], Value::String("b".into()));
        assert_eq!(parse(&text, &[]).unwrap().values, document.values);
    }

    #[test]
    fn a_setting_can_share_a_group_with_nested_groups() {
        let text = "/x/y:\n  type: b\n  value: 'true'\n";
        let nested = "x:\n  y:\n    type: b\n    value: 'true'\n  z:\n    w:\n      type: s\n      value: '\"hi\"'\n";
        assert_eq!(parse(nested, &[]).unwrap().values.len(), 2);
        assert!(parse(text, &[]).unwrap().values.is_empty(), "a path with a leading slash is not a key");
    }

    #[test]
    fn a_string_value_is_stored_as_text_not_as_yaml() {
        let mut document = Document::default();
        document.values.insert("/test/numeric-text".to_owned(), "1200".to_variant());
        let text = serialize(&document).unwrap();
        assert!(text.contains("type: s"), "the type is written: {text}");
        assert_eq!(parse(&text, &[]).unwrap().values, document.values);
    }

    #[test]
    fn an_entry_whose_value_is_not_text_is_kept_but_ignored() {
        let text = "test:\n  size:\n    type: i\n    value: 'wide'\n  flag:\n    type: b\n    value: true\n";
        let document = parse(text, &[]).unwrap();
        assert!(document.values.is_empty(), "neither entry is a valid setting");
        assert!(document.invalid.contains_key("/test/size"));
        assert!(document.invalid.contains_key("/test/flag"));
        let written = serialize(&document).unwrap();
        assert!(written.contains("wide"), "the malformed entry is written back unchanged");
    }

    #[test]
    fn an_unknown_type_signature_is_invalid() {
        let document = parse("test:\n  key:\n    type: zz\n    value: '1'\n", &[]).unwrap();
        assert!(document.values.is_empty());
        assert!(document.invalid.contains_key("/test/key"));
    }

    #[test]
    fn a_file_that_is_not_yaml_is_an_error() {
        assert!(parse("window: [", &[]).is_err());
        assert!(parse("- just\n- a list\n", &[]).is_err());
    }

    #[test]
    fn an_empty_file_holds_no_settings() {
        assert!(parse("", &[]).unwrap().values.is_empty());
    }

    #[test]
    fn a_missing_file_holds_no_settings() {
        let file = std::env::temp_dir().join(format!("rsimgv-missing-{}", std::process::id())).join("settings.yaml");
        assert!(load(&file, &[]).unwrap().values.is_empty());
    }

    #[test]
    fn a_save_creates_the_missing_parent_directory() {
        let dir = std::env::temp_dir().join(format!("rsimgv-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("settings.yaml");
        let mut document = Document::default();
        document.values.insert("/test/flag".to_owned(), true.to_variant());
        save(&file, &document).unwrap();
        assert_eq!(load(&file, &[]).unwrap().values, document.values);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_file_loads_with_the_schema_types() {
        let keys = [SchemaKey { path: "/test/window-width".to_owned(), name: "window-width".to_owned(), ty: "i".to_owned() }];
        let document = parse("window:\n  width: 960\n", &keys).unwrap();
        assert_eq!(document.values.get("/test/window-width"), Some(&960i32.to_variant()));
    }

    #[test]
    fn a_typed_setting_wins_over_the_same_legacy_setting() {
        let keys = [SchemaKey { path: "/test/window-width".to_owned(), name: "window-width".to_owned(), ty: "i".to_owned() }];
        let text = "window:\n  width: 960\ntest:\n  window-width:\n    type: i\n    value: '1024'\n";
        let document = parse(text, &keys).unwrap();
        assert_eq!(document.values.get("/test/window-width"), Some(&1024i32.to_variant()));
    }
}
