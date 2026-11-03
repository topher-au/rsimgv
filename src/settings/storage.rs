//! The settings file: one YAML document nested by the segments of each setting's GSettings key path below the root
//! path of the schema, to any depth. A setting is a mapping holding its exact GVariant `type` and its GVariant text as
//! `value`, so no YAML scalar is ever read as a setting's type. With the root `/au/topher/rsimgv/`, the settings at
//! `/au/topher/rsimgv/window/width` and `/au/topher/rsimgv/window/sidebar/width` are:
//!
//! ```yaml
//! window:
//!   width:
//!     type: i
//!     value: "960"
//!   sidebar:
//!     width:
//!       type: i
//!       value: "-1"
//! ```
//!
//! A mapping with `type` and `value` keys is always a setting, so those two names cannot be used for a group. An entry
//! that does not parse is kept as it was written, so a save never discards it, and it reads as absent.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use gtk::glib;
use serde_yaml_ng::{Mapping, Value};

/// The settings in a file.
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// The settings that parse, by full key path.
    pub values: BTreeMap<String, glib::Variant>,
    /// The entries that do not parse, by full key path, as they were written.
    pub invalid: BTreeMap<String, Value>,
}

/// Reads the document in `text`, whose top level is at the key path `root`.
pub fn parse(text: &str, root: &str) -> anyhow::Result<Document> {
    let top = match serde_yaml_ng::from_str::<Value>(text).context("the settings are not valid YAML")? {
        Value::Null => Mapping::new(),
        Value::Mapping(top) => top,
        _ => bail!("the top level of the settings is not a mapping"),
    };

    let mut document = Document::default();
    walk(&top, root, &mut document);
    Ok(document)
}

/// Reads the mapping `node`, at the key path `prefix`, into `document`. A mapping that is not a setting is a group.
fn walk(node: &Mapping, prefix: &str, document: &mut Document) {
    for (key, value) in node {
        let Some(name) = key.as_str() else {
            eprintln!("rsimgv: skipping a settings entry whose key is not a string");
            continue;
        };
        let path = format!("{prefix}{name}");
        match value {
            Value::Mapping(inner) if !is_entry(inner) => walk(inner, &format!("{path}/"), document),
            _ => match entry(value) {
                Some(variant) => {
                    document.values.insert(path, variant);
                }
                None => {
                    eprintln!("rsimgv: the setting {path} is not a typed value; it is kept as written and ignored");
                    document.invalid.insert(path, value.clone());
                }
            },
        }
    }
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

/// The YAML text of `document`, whose top level is at the key path `root`. A setting outside `root` is an error.
pub fn serialize(document: &Document, root: &str) -> anyhow::Result<String> {
    let mut top = Mapping::new();
    for (path, variant) in &document.values {
        let mut entry = Mapping::new();
        entry.insert(Value::String("type".to_owned()), Value::String(variant.type_().as_str().to_owned()));
        entry.insert(Value::String("value".to_owned()), Value::String(variant.print(false).to_string()));
        insert_at(&mut top, relative(path, root)?, Value::Mapping(entry));
    }
    for (path, value) in &document.invalid {
        insert_at(&mut top, relative(path, root)?, value.clone());
    }
    serde_yaml_ng::to_string(&Value::Mapping(top)).context("the settings could not be written as YAML")
}

/// The key path `path` below `root`.
fn relative<'a>(path: &'a str, root: &str) -> anyhow::Result<&'a str> {
    path.strip_prefix(root).with_context(|| format!("the setting {path} is not under {root}"))
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
pub fn load(file: &Path, root: &str) -> anyhow::Result<Document> {
    match std::fs::read_to_string(file) {
        Ok(text) => parse(&text, root).with_context(|| format!("could not read {}", file.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Document::default()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Writes `document` to `file` as one atomic replacement, creating the parent directory if it is missing.
pub fn save(file: &Path, document: &Document, root: &str) -> anyhow::Result<()> {
    let text = serialize(document, root)?;
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

    /// The root path of the settings in the tests.
    const ROOT: &str = "/test/";

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
        let text = serialize(&document, ROOT).unwrap();
        let parsed = parse(&text, ROOT).unwrap();
        assert_eq!(parsed.values, document.values);
        assert!(parsed.invalid.is_empty());
    }

    #[test]
    fn settings_nest_to_any_depth_below_the_root() {
        let mut document = Document::default();
        document.values.insert("/test/window/width".to_owned(), 1200i32.to_variant());
        document.values.insert("/test/a/b/c/d/e/deep".to_owned(), true.to_variant());
        document.values.insert("/test/top".to_owned(), "x".to_variant());
        let text = serialize(&document, ROOT).unwrap();
        let top: Value = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(top["window"]["width"]["value"], Value::String("1200".into()));
        assert_eq!(top["a"]["b"]["c"]["d"]["e"]["deep"]["type"], Value::String("b".into()));
        assert!(top.get("test").is_none(), "the root path is not in the file");
        assert_eq!(parse(&text, ROOT).unwrap().values, document.values);
    }

    #[test]
    fn a_setting_outside_the_root_cannot_be_written() {
        let mut document = Document::default();
        document.values.insert("/elsewhere/key".to_owned(), true.to_variant());
        assert!(serialize(&document, ROOT).is_err());
    }

    #[test]
    fn a_scalar_is_kept_but_ignored() {
        let document = parse("window:\n  width: 960\n", ROOT).unwrap();
        assert!(document.values.is_empty());
        assert_eq!(document.invalid.get("/test/window/width"), Some(&Value::Number(960.into())));
        assert!(serialize(&document, ROOT).unwrap().contains("width: 960"), "it is written back unchanged");
    }

    #[test]
    fn a_setting_can_share_a_group_with_nested_groups() {
        let text = "/x/y:\n  type: b\n  value: 'true'\n";
        let nested = "x:\n  y:\n    type: b\n    value: 'true'\n  z:\n    w:\n      type: s\n      value: '\"hi\"'\n";
        assert_eq!(parse(nested, ROOT).unwrap().values.len(), 2);
        assert!(parse(text, ROOT).unwrap().values.is_empty(), "a path with a leading slash is not a key");
    }

    #[test]
    fn a_string_value_is_stored_as_text_not_as_yaml() {
        let mut document = Document::default();
        document.values.insert("/test/numeric-text".to_owned(), "1200".to_variant());
        let text = serialize(&document, ROOT).unwrap();
        assert!(text.contains("type: s"), "the type is written: {text}");
        assert_eq!(parse(&text, ROOT).unwrap().values, document.values);
    }

    #[test]
    fn an_entry_whose_value_is_not_text_is_kept_but_ignored() {
        let text = "test:\n  size:\n    type: i\n    value: 'wide'\n  flag:\n    type: b\n    value: true\n";
        let document = parse(text, ROOT).unwrap();
        assert!(document.values.is_empty(), "neither entry is a valid setting");
        assert!(document.invalid.contains_key("/test/test/size"));
        assert!(document.invalid.contains_key("/test/test/flag"));
        let written = serialize(&document, ROOT).unwrap();
        assert!(written.contains("wide"), "the malformed entry is written back unchanged");
    }

    #[test]
    fn an_unknown_type_signature_is_invalid() {
        let document = parse("test:\n  key:\n    type: zz\n    value: '1'\n", ROOT).unwrap();
        assert!(document.values.is_empty());
        assert!(document.invalid.contains_key("/test/test/key"));
    }

    #[test]
    fn a_file_that_is_not_yaml_is_an_error() {
        assert!(parse("window: [", ROOT).is_err());
        assert!(parse("- just\n- a list\n", ROOT).is_err());
    }

    #[test]
    fn an_empty_file_holds_no_settings() {
        assert!(parse("", ROOT).unwrap().values.is_empty());
    }

    #[test]
    fn a_missing_file_holds_no_settings() {
        let file = std::env::temp_dir().join(format!("rsimgv-missing-{}", std::process::id())).join("settings.yaml");
        assert!(load(&file, ROOT).unwrap().values.is_empty());
    }

    #[test]
    fn a_save_creates_the_missing_parent_directory() {
        let dir = std::env::temp_dir().join(format!("rsimgv-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("settings.yaml");
        let mut document = Document::default();
        document.values.insert("/test/flag".to_owned(), true.to_variant());
        save(&file, &document, ROOT).unwrap();
        assert_eq!(load(&file, ROOT).unwrap().values, document.values);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
