//! The sort chosen for each folder, kept while `remember-folder-sort` is on. They are one YAML file,
//! `folder_sort.yaml` in the config directory, with an entry for each folder by its path:
//!
//! ```yaml
//! /home/me/Pictures:
//!   by: modified
//!   order: descending
//! ```

use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

use crate::settings::config_path;

/// How a folder is sorted, as the values of the `sort-by` and `sort-order` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSort {
    pub by: String,
    pub order: String,
}

/// Where the folders' sorts are kept.
fn sort_file() -> PathBuf {
    config_path().with_file_name("folder_sort.yaml")
}

/// The sort remembered for `folder`, if there is one.
pub fn remembered(folder: &Path) -> Option<FolderSort> {
    read_entry(&sort_file(), folder)
}

/// Remembers `sort` for `folder`.
pub fn remember(folder: &Path, sort: &FolderSort) -> std::io::Result<()> {
    write_entry(&sort_file(), folder, sort)
}

/// The mapping in `file`, or none if it is missing or does not parse.
fn read_mapping(file: &Path) -> Mapping {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_yaml_ng::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_mapping().cloned())
        .unwrap_or_default()
}

/// The entry for `folder` in `file`. An entry the settings would not accept is taken as no entry.
fn read_entry(file: &Path, folder: &Path) -> Option<FolderSort> {
    let entries = read_mapping(file);
    let entry = entries.get(folder.to_string_lossy().as_ref())?;
    let by = entry.get("by")?.as_str()?;
    let order = entry.get("order")?.as_str()?;
    let valid = ["name", "modified", "size", "type"].contains(&by) && ["ascending", "descending"].contains(&order);
    valid.then(|| FolderSort { by: by.to_owned(), order: order.to_owned() })
}

/// Sets the entry for `folder` in `file`, onto what the file holds now, since another window may have saved others.
fn write_entry(file: &Path, folder: &Path, sort: &FolderSort) -> std::io::Result<()> {
    let mut entry = Mapping::new();
    entry.insert("by".into(), sort.by.as_str().into());
    entry.insert("order".into(), sort.order.as_str().into());
    let mut entries = read_mapping(file);
    entries.insert(folder.to_string_lossy().as_ref().into(), Value::Mapping(entry));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_yaml_ng::to_string(&Value::Mapping(entries)).map_err(std::io::Error::other)?;
    crate::atomic_file::write(file, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sort(by: &str, order: &str) -> FolderSort {
        FolderSort { by: by.to_owned(), order: order.to_owned() }
    }

    #[test]
    fn each_folder_keeps_its_own_sort() {
        let file = std::env::temp_dir().join(format!("rsimgv-folder-sort-{}", std::process::id())).join("folder_sort.yaml");
        let (first, second) = (Path::new("/pictures"), Path::new("/videos"));
        assert_eq!(read_entry(&file, first), None, "a folder starts with none");

        write_entry(&file, first, &sort("size", "descending")).unwrap();
        write_entry(&file, second, &sort("type", "ascending")).unwrap();
        write_entry(&file, first, &sort("modified", "descending")).unwrap();
        assert_eq!(read_entry(&file, first), Some(sort("modified", "descending")));
        assert_eq!(read_entry(&file, second), Some(sort("type", "ascending")));

        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn an_entry_the_settings_would_not_accept_is_ignored() {
        let file = std::env::temp_dir().join(format!("rsimgv-folder-sort-bad-{}.yaml", std::process::id()));
        std::fs::write(&file, "/a:\n  by: colour\n  order: ascending\n/b:\n  by: name\n").unwrap();
        assert_eq!(read_entry(&file, Path::new("/a")), None);
        assert_eq!(read_entry(&file, Path::new("/b")), None);
        let _ = std::fs::remove_file(&file);
    }
}
