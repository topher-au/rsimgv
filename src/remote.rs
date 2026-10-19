//! The IPC methods, run on the GTK thread against the window. Each takes JSON parameters and returns JSON.
//!
//!   actions.list    {}                       -> {"actions": [{"name", "label", "description", "scope", "icon", "defaults"}, ...]}
//!   actions.run     {"name"}                 -> {}
//!   window.info     {}                       -> {"current_path", "selected_file", "show_sidebar"}
//!   window.set_show_sidebar {"show"}         -> {}
//!   tabs.list       {}                       -> {"tabs": [{"id", "index", "path", "active"}, ...]}
//!   tabs.current    {}                       -> {"id", "index"}   both null when no tab is open
//!   tabs.open       {"path", "activate"?}    -> {"id", "index"}   activate defaults to false
//!   tabs.activate   {"index"} or {"id"}     -> {}
//!   tabs.close      {"index"}                -> {}
//!   tabs.files      {"tab"}                  -> {"files": [{"name", "path", "is_dir"}, ...]}
//!   tabs.select     {"tab", "file"}          -> {"selected": name}   file is a name or a path
//!   bookmarks.add   {"path", "name"?}        -> {}          name defaults to the folder's own
//!   bookmarks.remove {"path"}                -> {}
//!   bookmarks.list  {}                       -> {"bookmarks": [{"name", "path"}, ...]}
//!   navigate.back   {"n"?}                   -> {"moved"}   n defaults to 1
//!   navigate.forward {"n"?}                  -> {"moved"}
//!   navigate.up     {"n"?}                   -> {"moved"}   stops at the root
//!   navigate.to     {"path"}                 -> {}
//!   navigate.next   {}                       -> {"path"}   the next folder in tree order, or null at the end
//!   navigate.previous {}                     -> {"path"}   the previous folder in tree order, or null at the start
//!   navigate.next_media {}                   -> {"path"}   the next folder with a picture or video, or null
//!   navigate.previous_media {}               -> {"path"}   the previous folder with a picture or video, or null
//!   files.list      {"path"?}                -> {"files": {name: {"path", "is_dir", "size", "modified"}, ...}}
//!                   With no path, the selected tab's items, in the order the tab shows them. With a path, the
//!                   folder's contents from disk, including hidden files, sorted by name.
//!   files.select    {"file"}                 -> {"selected": name}   in the selected tab
//!   files.select_index {"index"}             -> {"selected": name}   negative counts from the end
//!   files.selected  {}                       -> {"name", "index"}   of the selected tab; null when nothing is selected
//!
//! "tab" in tabs.files and tabs.select is a tab id, or "current" for the selected tab. "index" is a tab's
//! position, counted from 0 in tab order.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use crate::bookmarks;
use crate::ipc::Error;
use crate::keybindings::actions::{find, ACTIONS};
use crate::ui::Window;

pub fn handle(window: &Window, method: &str, params: &Value) -> Result<Value, Error> {
    match method {
        "actions.list" => {
            let actions: Vec<Value> = ACTIONS
                .iter()
                .map(|action| {
                    json!({
                        "name": action.name,
                        "label": action.label,
                        "description": action.description,
                        "scope": action.scope.id(),
                        "icon": action.icon,
                        "defaults": action.defaults,
                    })
                })
                .collect();
            Ok(json!({"actions": actions}))
        }
        "actions.run" => {
            let name = text_param(params, "name")?;
            if find(name).is_none() {
                return Err(Error::new("unknown_action", format!("no action named {name}")));
            }
            if window.run_action(name) {
                Ok(json!({}))
            } else {
                Err(Error::new("not_run", format!("the window did not run {name}")))
            }
        }
        "window.info" => Ok(json!({
            "current_path": window.current_path().map(|path| path.to_string_lossy().into_owned()),
            "selected_file": window.selected_file(),
            "show_sidebar": window.show_sidebar(),
        })),
        "window.set_show_sidebar" => {
            let shown = bool_param(params, "show")?;
            window.set_show_sidebar(shown);
            Ok(json!({}))
        }
        "tabs.list" => {
            let tabs: Vec<Value> = window
                .tabs()
                .iter()
                .map(|tab| {
                    json!({
                        "id": tab.id,
                        "index": tab.index,
                        "path": tab.path.as_ref().map(|path| path.to_string_lossy()),
                        "active": tab.active,
                    })
                })
                .collect();
            Ok(json!({"tabs": tabs}))
        }
        "tabs.current" => {
            let index = window.current_index();
            let id = index.and_then(|index| window.tab_id_at(index));
            Ok(json!({"id": id, "index": index}))
        }
        "tabs.open" => {
            let path = Path::new(text_param(params, "path")?);
            if !path.is_dir() {
                return Err(Error::new("not_a_folder", format!("{} is not a folder", path.display())));
            }
            let activate = params.get("activate").and_then(Value::as_bool).unwrap_or(false);
            let id = window.open_tab_at(path, activate);
            let index = window.tabs().iter().find(|tab| tab.id == id).map_or(0, |tab| tab.index);
            Ok(json!({"id": id, "index": index}))
        }
        "tabs.activate" => {
            let id = if params.get("id").is_some() { id_param(window, params)? } else { index_param(window, params)? };
            window.activate_tab(id);
            Ok(json!({}))
        }
        "tabs.close" => {
            let id = index_param(window, params)?;
            window.close_tab(id);
            Ok(json!({}))
        }
        "tabs.files" => {
            let id = tab_param(window, params)?;
            let files = window.tab_files(id).ok_or_else(|| no_tab(id))?;
            let files: Vec<Value> = files
                .iter()
                .map(|file| json!({"name": file.name, "path": file.path.to_string_lossy(), "is_dir": file.is_dir}))
                .collect();
            Ok(json!({"files": files}))
        }
        "tabs.select" => {
            let id = tab_param(window, params)?;
            let file = text_param(params, "file")?;
            match window.select_in_tab(id, file) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item {file} in that tab"))),
            }
        }
        "navigate.back" | "navigate.forward" => {
            let steps = count_param(params, "n")?;
            let forward = method == "navigate.forward";
            let moved = (0..steps).take_while(|_| window.step_history(forward)).count();
            Ok(json!({"moved": moved}))
        }
        "navigate.up" => {
            let levels = count_param(params, "n")?;
            Ok(json!({"moved": window.go_up_levels(levels)}))
        }
        "navigate.next" | "navigate.previous" => {
            let forward = method == "navigate.next";
            let path = window.step_folder(forward).map(|path| path.to_string_lossy().into_owned());
            Ok(json!({"path": path}))
        }
        "navigate.next_media" | "navigate.previous_media" => {
            let forward = method == "navigate.next_media";
            let path = window.step_media_folder(forward).map(|path| path.to_string_lossy().into_owned());
            Ok(json!({"path": path}))
        }
        "navigate.to" => {
            let path = Path::new(text_param(params, "path")?);
            if window.navigate(path) {
                Ok(json!({}))
            } else {
                Err(Error::new("not_a_folder", format!("{} is not a folder", path.display())))
            }
        }
        "files.list" => {
            let files = match params.get("path") {
                None | Some(Value::Null) => {
                    let id = tab_param(window, &json!({"tab": "current"}))?;
                    let listed = window.tab_files(id).ok_or_else(|| no_tab(id))?;
                    listed.into_iter().map(|file| (file.name, file.path)).collect()
                }
                Some(_) => folder_entries(Path::new(text_param(params, "path")?))?,
            };
            let mut map = serde_json::Map::new();
            for (name, path) in files {
                map.insert(name, file_info(&path));
            }
            Ok(json!({"files": map}))
        }
        "files.select" => {
            let id = tab_param(window, &json!({"tab": "current"}))?;
            let file = text_param(params, "file")?;
            match window.select_in_tab(id, file) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item {file} in the selected tab"))),
            }
        }
        "files.selected" => Ok(json!({
            "name": window.selected_file(),
            "index": window.selected_position(),
        })),
        "files.select_index" => {
            let index = params
                .get("index")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::new("invalid_params", "the parameter index must be a whole number"))?;
            match window.select_at_index(index) {
                Some(name) => Ok(json!({"selected": name})),
                None => Err(Error::new("no_such_file", format!("no item at index {index}"))),
            }
        }
        "bookmarks.add" => {
            let path = Path::new(text_param(params, "path")?);
            let name = match params.get("name") {
                None | Some(Value::Null) => None,
                Some(_) => Some(text_param(params, "name")?.to_owned()),
            };
            window.add_bookmark(path, name);
            Ok(json!({}))
        }
        "bookmarks.list" => {
            let bookmarks = bookmarks::with(|list| {
                list.iter()
                    .map(|bookmark| json!({"name": bookmark.name, "path": bookmark.path.to_string_lossy()}))
                    .collect::<Vec<Value>>()
            });
            Ok(json!({"bookmarks": bookmarks}))
        }
        "bookmarks.remove" => {
            let path = Path::new(text_param(params, "path")?);
            window.remove_bookmark(path);
            Ok(json!({}))
        }
        _ => Err(Error::new("unknown_method", format!("no method named {method}"))),
    }
}

fn text_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, Error> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be text")))
}

fn bool_param(params: &Value, key: &str) -> Result<bool, Error> {
    params
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be true or false")))
}

/// The id of the tab at the position given by the request's "index".
fn index_param(window: &Window, params: &Value) -> Result<u64, Error> {
    let index = params
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("invalid_params", "the parameter index must be a tab position"))?;
    window
        .tab_id_at(index as usize)
        .ok_or_else(|| Error::new("no_such_tab", format!("no tab at index {index}")))
}

/// The id of the tab named by the request's "id", which must be an open tab.
fn id_param(window: &Window, params: &Value) -> Result<u64, Error> {
    let id = params
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("invalid_params", "the parameter id must be a tab id"))?;
    if window.has_tab(id) {
        Ok(id)
    } else {
        Err(no_tab(id))
    }
}

/// The tab a request names: its id, or the selected tab for "current".
fn tab_param(window: &Window, params: &Value) -> Result<u64, Error> {
    match params.get("tab") {
        Some(Value::String(text)) if text.as_str() == "current" => {
            let index = window.current_index().ok_or_else(|| Error::new("no_such_tab", "no tab is open"))?;
            window.tab_id_at(index).ok_or_else(|| Error::new("no_such_tab", "no tab is open"))
        }
        Some(value) => {
            let id = value
                .as_u64()
                .ok_or_else(|| Error::new("invalid_params", "tab must be an id or \"current\""))?;
            if window.has_tab(id) {
                Ok(id)
            } else {
                Err(no_tab(id))
            }
        }
        None => Err(Error::new("invalid_params", "the parameter tab is required")),
    }
}

/// A count parameter, such as the steps of a navigation. Defaults to 1 when absent.
fn count_param(params: &Value, key: &str) -> Result<u32, Error> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(1),
        Some(value) => value
            .as_u64()
            .and_then(|count| u32::try_from(count).ok())
            .ok_or_else(|| Error::new("invalid_params", format!("the parameter {key} must be a whole number"))),
    }
}

/// The name and path of each item in a folder on disk, sorted by name, hidden items included.
fn folder_entries(folder: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let read = std::fs::read_dir(folder)
        .map_err(|err| Error::new("not_a_folder", format!("cannot list {}: {err}", folder.display())))?;
    let mut entries: Vec<(String, PathBuf)> = read
        .flatten()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

/// What the IPC says about one item: its path, whether it is a folder, its size in bytes for a file, and when it
/// was last modified, in seconds since the Unix epoch.
fn file_info(path: &Path) -> Value {
    let metadata = std::fs::metadata(path).ok();
    let is_dir = metadata.as_ref().is_some_and(|metadata| metadata.is_dir());
    let size = metadata.as_ref().filter(|metadata| !metadata.is_dir()).map(|metadata| metadata.len());
    let modified = metadata
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs_f64());
    json!({"path": path.to_string_lossy(), "is_dir": is_dir, "size": size, "modified": modified})
}

fn no_tab(id: u64) -> Error {
    Error::new("no_such_tab", format!("no tab {id}"))
}
