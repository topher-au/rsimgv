//! The settings file: one YAML document nested by the segments of each setting's GSettings key path below the root
//! path of the schema, to any depth. A setting holds its plain value, with its type left to the schema. With the root
//! `/au/topher/rsimgv/`, the settings at `/au/topher/rsimgv/window/width` and `/au/topher/rsimgv/window/sidebar/width` are:
//!
//! ```yaml
//! window:
//!   width: 960
//!   sidebar:
//!     width: -1
//! ```
//!
//! A mapping is always a group. Booleans, numbers and strings are YAML scalars, and arrays and tuples of them are
//! sequences. A value of any other type, such as a dictionary, is a string holding its GVariant text.
//!
//! The file is read without the schema, and each value takes its type when GSettings asks for it, which says the type.
//! A value that does not fit reads as absent, and stays as it was written, so a save never discards it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use glib::prelude::ToVariant;
use gtk::glib;
use serde_yaml_ng::{Mapping, Value};

/// The settings in a file, by full key path, as they were written.
pub type Document = BTreeMap<String, Value>;

/// Reads the document in `text`, whose top level is at the key path `root`.
pub fn parse(text: &str, root: &str) -> anyhow::Result<Document> {
    let top = match serde_yaml_ng::from_str::<Value>(text).context("the settings are not valid YAML")? {
        Value::Null => Mapping::new(),
        Value::Mapping(top) => top,
        _ => bail!("the top level of the settings is not a mapping"),
    };

    let mut document = Document::new();
    walk(&top, root, &mut document);
    Ok(document)
}

/// Reads the mapping `node`, at the key path `prefix`, into `document`.
fn walk(node: &Mapping, prefix: &str, document: &mut Document) {
    for (key, value) in node {
        let Some(name) = key.as_str() else {
            eprintln!("rsimgv: skipping a settings entry whose key is not a string");
            continue;
        };
        let path = format!("{prefix}{name}");
        match value {
            Value::Mapping(inner) => walk(inner, &format!("{path}/"), document),
            _ => {
                document.insert(path, value.clone());
            }
        }
    }
}

/// Whether values of type `ty` are written as plain YAML rather than as GVariant text.
fn is_plain(ty: &glib::VariantTy) -> bool {
    match ty.as_str() {
        "b" | "y" | "n" | "q" | "i" | "u" | "x" | "t" | "d" | "s" => true,
        _ if ty.is_array() => is_plain(ty.element()),
        _ if ty.is_tuple() => members(ty).all(is_plain),
        _ => false,
    }
}

/// The types of the members of the tuple type `ty`.
fn members(ty: &glib::VariantTy) -> impl Iterator<Item = &glib::VariantTy> {
    std::iter::successors(ty.first(), |member| member.next())
}

/// The value of `value` as type `ty`, if it fits. A number that is out of range for `ty` does not fit, and neither does
/// a YAML scalar of another kind, except that a whole number fits a double.
pub fn to_variant(value: &Value, ty: &glib::VariantTy) -> Option<glib::Variant> {
    if !is_plain(ty) {
        return glib::Variant::parse(Some(ty), value.as_str()?).ok();
    }
    match ty.as_str() {
        "b" => value.as_bool().map(|b| b.to_variant()),
        "y" => value.as_u64().and_then(|n| u8::try_from(n).ok()).map(|n| n.to_variant()),
        "n" => value.as_i64().and_then(|n| i16::try_from(n).ok()).map(|n| n.to_variant()),
        "q" => value.as_u64().and_then(|n| u16::try_from(n).ok()).map(|n| n.to_variant()),
        "i" => value.as_i64().and_then(|n| i32::try_from(n).ok()).map(|n| n.to_variant()),
        "u" => value.as_u64().and_then(|n| u32::try_from(n).ok()).map(|n| n.to_variant()),
        "x" => value.as_i64().map(|n| n.to_variant()),
        "t" => value.as_u64().map(|n| n.to_variant()),
        "d" => value.as_f64().map(|n| n.to_variant()),
        "s" => value.as_str().map(|s| s.to_variant()),
        _ if ty.is_array() => {
            let items: Option<Vec<_>> = value.as_sequence()?.iter().map(|item| to_variant(item, ty.element())).collect();
            Some(glib::Variant::array_from_iter_with_type(ty.element(), items?))
        }
        _ => {
            let items = value.as_sequence()?;
            if items.len() != ty.n_items() {
                return None;
            }
            let items: Option<Vec<_>> = items.iter().zip(members(ty)).map(|(item, member)| to_variant(item, member)).collect();
            Some(glib::Variant::tuple_from_iter(items?))
        }
    }
}

/// The value to write for `variant`.
pub fn from_variant(variant: &glib::Variant) -> Value {
    Some(variant)
        .filter(|variant| is_plain(variant.type_()))
        .and_then(plain)
        .unwrap_or_else(|| Value::String(variant.print(false).to_string()))
}

/// The plain YAML of `variant`, whose type is one `is_plain` accepts.
fn plain(variant: &glib::Variant) -> Option<Value> {
    match variant.type_().as_str() {
        "b" => variant.get::<bool>().map(Value::from),
        "y" => variant.get::<u8>().map(Value::from),
        "n" => variant.get::<i16>().map(Value::from),
        "q" => variant.get::<u16>().map(Value::from),
        "i" => variant.get::<i32>().map(Value::from),
        "u" => variant.get::<u32>().map(Value::from),
        "x" => variant.get::<i64>().map(Value::from),
        "t" => variant.get::<u64>().map(Value::from),
        "d" => variant.get::<f64>().map(Value::from),
        "s" => variant.str().map(Value::from),
        _ => variant.iter().map(|item| plain(&item)).collect::<Option<Vec<_>>>().map(Value::Sequence),
    }
}

/// The YAML text of `document`, whose top level is at the key path `root`. A setting outside `root` is an error.
pub fn serialize(document: &Document, root: &str) -> anyhow::Result<String> {
    let mut top = Mapping::new();
    for (path, value) in document {
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

    /// The root path of the settings in the tests.
    const ROOT: &str = "/test/";

    fn ty(signature: &str) -> &glib::VariantTy {
        glib::VariantTy::new(signature).unwrap()
    }

    /// The YAML value written as `text`.
    fn yaml(text: &str) -> Value {
        serde_yaml_ng::from_str(text).unwrap()
    }

    /// One value of each type the schemas could use, as GVariant text, with the tricky strings that need escaping.
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
        ("d", "2.0"),
        ("s", "'quote \" and \\\\ backslash\\nnewline ünïcödé'"),
        ("s", "'1200'"),
        ("s", "'true'"),
        ("s", "''"),
        ("o", "'/org/example/object'"),
        ("g", "'a{sv}'"),
        ("as", "['size', 'modified']"),
        ("as", "@as []"),
        ("ai", "[1, -2, 3]"),
        ("(is)", "(7, 'seven')"),
        ("(sb)", "('', false)"),
        ("a(is)", "[(1, 'one')]"),
        ("a{sv}", "{'speed': <1.25>, 'name': <'x'>}"),
        ("a{sv}", "@a{sv} {}"),
        ("mi", "just 3"),
        ("ms", "nothing"),
    ];

    #[test]
    fn every_type_round_trips_through_the_file() {
        let mut document = Document::new();
        let variants: Vec<(&glib::VariantTy, glib::Variant)> =
            SAMPLES.iter().map(|(signature, text)| (ty(signature), glib::Variant::parse(Some(ty(signature)), text).unwrap())).collect();
        for (index, (_, variant)) in variants.iter().enumerate() {
            document.insert(format!("/test/key-{index}"), from_variant(variant));
        }
        let parsed = parse(&serialize(&document, ROOT).unwrap(), ROOT).unwrap();
        for (index, (ty, variant)) in variants.iter().enumerate() {
            let value = &parsed[&format!("/test/key-{index}")];
            assert_eq!(to_variant(value, ty).as_ref(), Some(variant), "{} as {}", value.as_str().unwrap_or("a plain value"), ty.as_str());
        }
    }

    #[test]
    fn values_are_written_as_plain_yaml() {
        let written = |signature: &str, text: &str| from_variant(&glib::Variant::parse(Some(ty(signature)), text).unwrap());
        assert_eq!(written("b", "true"), yaml("true"));
        assert_eq!(written("i", "960"), yaml("960"));
        assert_eq!(written("d", "0.5"), yaml("0.5"));
        assert_eq!(written("s", "'mpv'"), yaml("mpv"));
        assert_eq!(written("as", "['size', 'modified']"), yaml("[size, modified]"));
        assert_eq!(written("(sb)", "('name', true)"), yaml("[name, true]"));
        assert_eq!(written("a{si}", "{'a': 1}"), Value::String("{'a': 1}".to_owned()), "a dictionary is its GVariant text");
    }

    #[test]
    fn a_value_that_does_not_fit_its_type_gives_nothing() {
        for (signature, text) in [
            ("i", "wide"),
            ("i", "'960'"),
            ("i", "true"),
            ("i", "1.5"),
            ("i", "3000000000"),
            ("y", "-1"),
            ("u", "-1"),
            ("b", "1"),
            ("b", "'true'"),
            ("s", "1200"),
            ("s", "true"),
            ("as", "size"),
            ("as", "[1]"),
            ("(sb)", "[name]"),
            ("(sb)", "[name, true, more]"),
            ("a{si}", "not a dictionary"),
        ] {
            assert_eq!(to_variant(&yaml(text), ty(signature)), None, "{text} as {signature}");
        }
    }

    #[test]
    fn a_whole_number_fits_a_double() {
        assert_eq!(to_variant(&yaml("2"), ty("d")), Some(2.0f64.to_variant()));
    }

    #[test]
    fn settings_nest_to_any_depth_below_the_root() {
        let mut document = Document::new();
        document.insert("/test/window/width".to_owned(), yaml("1200"));
        document.insert("/test/a/b/c/d/e/deep".to_owned(), yaml("true"));
        document.insert("/test/top".to_owned(), yaml("x"));
        let text = serialize(&document, ROOT).unwrap();
        let top = yaml(&text);
        assert_eq!(top["window"]["width"], yaml("1200"));
        assert_eq!(top["a"]["b"]["c"]["d"]["e"]["deep"], yaml("true"));
        assert!(top.get("test").is_none(), "the root path is not in the file");
        assert_eq!(parse(&text, ROOT).unwrap(), document);
    }

    #[test]
    fn a_setting_outside_the_root_cannot_be_written() {
        let mut document = Document::new();
        document.insert("/elsewhere/key".to_owned(), yaml("true"));
        assert!(serialize(&document, ROOT).is_err());
    }

    #[test]
    fn a_value_is_kept_as_it_was_written() {
        let document = parse("window:\n  width: wide\n", ROOT).unwrap();
        assert_eq!(document.get("/test/window/width"), Some(&yaml("wide")));
        assert!(serialize(&document, ROOT).unwrap().contains("width: wide"), "it is written back unchanged");
    }

    #[test]
    fn a_file_that_is_not_yaml_is_an_error() {
        assert!(parse("window: [", ROOT).is_err());
        assert!(parse("- just\n- a list\n", ROOT).is_err());
    }

    #[test]
    fn an_empty_file_holds_no_settings() {
        assert!(parse("", ROOT).unwrap().is_empty());
    }

    #[test]
    fn a_missing_file_holds_no_settings() {
        let file = std::env::temp_dir().join(format!("rsimgv-missing-{}", std::process::id())).join("settings.yaml");
        assert!(load(&file, ROOT).unwrap().is_empty());
    }

    #[test]
    fn a_save_creates_the_missing_parent_directory() {
        let dir = std::env::temp_dir().join(format!("rsimgv-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested").join("settings.yaml");
        let mut document = Document::new();
        document.insert("/test/flag".to_owned(), yaml("true"));
        save(&file, &document, ROOT).unwrap();
        assert_eq!(load(&file, ROOT).unwrap(), document);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
