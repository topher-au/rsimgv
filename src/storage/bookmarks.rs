//! Bookmarked folders, kept in `bookmarks.yaml` beside the settings file. Pure: no GTK.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

use crate::settings::config_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
}

thread_local! {
    static BOOKMARKS: RefCell<Vec<Bookmark>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` on the live list. Do not call `with` again inside `f`.
pub fn with<R>(f: impl FnOnce(&mut Vec<Bookmark>) -> R) -> R {
    BOOKMARKS.with(|list| f(&mut list.borrow_mut()))
}

/// Bookmarks `path` as `name`. A folder already bookmarked is renamed rather than added twice.
pub fn add(list: &mut Vec<Bookmark>, path: PathBuf, name: String) {
    match list.iter_mut().find(|bookmark| bookmark.path == path) {
        Some(existing) => existing.name = name,
        None => list.push(Bookmark { name, path }),
    }
}

/// Moves the bookmark at `from` to position `to`, among the others. A position past the end changes nothing.
pub fn move_to(list: &mut Vec<Bookmark>, from: usize, to: usize) {
    if from < list.len() && to < list.len() {
        let bookmark = list.remove(from);
        list.insert(to, bookmark);
    }
}

/// The name a folder gets when none is given: its last component.
pub fn default_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |name| name.to_string_lossy().into_owned())
}

pub fn to_yaml(list: &[Bookmark]) -> String {
    let items = list
        .iter()
        .map(|bookmark| {
            let mut item = Mapping::new();
            item.insert(Value::String("name".to_owned()), Value::String(bookmark.name.clone()));
            item.insert(Value::String("path".to_owned()), Value::String(bookmark.path.to_string_lossy().into_owned()));
            Value::Mapping(item)
        })
        .collect();
    let mut root = Mapping::new();
    root.insert(Value::String("schema".to_owned()), Value::Number(1.into()));
    root.insert(Value::String("bookmarks".to_owned()), Value::Sequence(items));
    serde_yaml_ng::to_string(&Value::Mapping(root)).unwrap_or_default()
}

/// Reads the bookmarks in `text`. An entry without a path is skipped; one without a name is named after its folder.
pub fn from_yaml(text: &str) -> Result<Vec<Bookmark>, String> {
    let root: Value = serde_yaml_ng::from_str(text).map_err(|err| err.to_string())?;
    let Some(items) = root.get("bookmarks").and_then(Value::as_sequence) else { return Ok(Vec::new()) };
    Ok(items
        .iter()
        .filter_map(|item| {
            let path = PathBuf::from(item.get("path")?.as_str()?);
            let name = item.get("name").and_then(Value::as_str).map_or_else(|| default_name(&path), str::to_owned);
            Some(Bookmark { name, path })
        })
        .collect())
}

/// The bookmarks file: `bookmarks.yaml` beside the settings file.
pub fn file() -> PathBuf {
    config_path().with_file_name("bookmarks.yaml")
}

/// Loads the bookmarks file, if there is one. A file that cannot be read is reported and leaves the list empty.
pub fn load() {
    let Ok(text) = std::fs::read_to_string(file()) else { return };
    match from_yaml(&text) {
        Ok(list) => with(|bookmarks| *bookmarks = list),
        Err(err) => eprintln!("rsimgv: could not read bookmarks: {err}"),
    }
}

/// Writes the bookmarks to their file.
pub fn save() -> io::Result<()> {
    let text = with(|bookmarks| to_yaml(bookmarks));
    let file = file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::storage::atomic_file::write(&file, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_round_trip_through_yaml() {
        let mut list = Vec::new();
        add(&mut list, PathBuf::from("/photos/2026"), "Trip".to_owned());
        add(&mut list, PathBuf::from("/music"), default_name(Path::new("/music")));
        assert_eq!(from_yaml(&to_yaml(&list)).unwrap(), list);
    }

    #[test]
    fn bookmarking_a_folder_twice_renames_it() {
        let mut list = Vec::new();
        add(&mut list, PathBuf::from("/photos"), "Photos".to_owned());
        add(&mut list, PathBuf::from("/photos"), "Holiday".to_owned());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Holiday");
    }

    #[test]
    fn moving_a_bookmark_keeps_the_others_in_order() {
        let mut list = Vec::new();
        for name in ["a", "b", "c"] {
            add(&mut list, PathBuf::from(format!("/{name}")), name.to_owned());
        }
        let names = |list: &[Bookmark]| list.iter().map(|bookmark| bookmark.name.clone()).collect::<Vec<_>>();
        move_to(&mut list, 0, 2);
        assert_eq!(names(&list), ["b", "c", "a"], "down");
        move_to(&mut list, 2, 1);
        assert_eq!(names(&list), ["b", "a", "c"], "up");
        move_to(&mut list, 0, 3);
        assert_eq!(names(&list), ["b", "a", "c"], "past the end changes nothing");
    }

    #[test]
    fn entries_without_a_path_are_skipped() {
        let list = from_yaml("bookmarks:\n  - name: Nowhere\n  - path: /srv/data\n").unwrap();
        assert_eq!(list, vec![Bookmark { name: "data".to_owned(), path: PathBuf::from("/srv/data") }]);
    }
}
